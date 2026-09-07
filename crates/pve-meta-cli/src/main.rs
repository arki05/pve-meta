//! `pve-meta`: the CLI, running the exact same `pve_meta_api::api` handler functions
//! in-process against the store (no HTTP, no daemon required).
//!
//! # Deviation from `docs/DAEMON-SPEC.md`
//!
//! The spec describes building this CLI declaratively on `proxmox_router::cli`
//! (`CliCommandMap`/`CliCommand::new(&API_METHOD_*).arg_param(...)`). Most of `docs/API.md`'s CLI
//! surface (`set k=v...`, `delete <path>`, `edit`, the `datacenter` sub-map) does not map onto a
//! single REST parameter schema — `set`/`delete`/`edit` all need CLI-only argument composition
//! (building a JSON patch from `k=v` pairs, or driving `$EDITOR`) before/after calling a shared
//! handler. Rather than mixing `CliCommand`-wrapped endpoints with several bespoke CLI-only
//! `#[api]` methods, this binary uses one small hand-rolled argument parser throughout and calls
//! the identical `pub fn` handlers in [`pve_meta_api::api`] directly — the business logic is
//! 100% shared with the HTTP daemon (same functions, same store, same validation), only the
//! declarative CLI-parsing framework is not used. This was a deliberate time/complexity trade-off
//! for a v1; see the implementation report.

use anyhow::{bail, Context as _, Error};
use serde_json::Value;

use proxmox_router::cli::CliEnvironment;
use proxmox_router::RpcEnvironment;

use pve_meta_api::api::{datacenter, guests, meta, registry};
use pve_meta_core::path::Path as DocPath;
use pve_meta_core::store::DocId;

fn cli_env() -> CliEnvironment {
    let mut env = CliEnvironment::new();
    env.set_auth_id(Some("root@pam".to_string()));
    env
}

fn usage() -> ! {
    eprintln!(
        "usage: pve-meta <command> [args...]\n\n\
         commands:\n\
         \x20 list [--has <path>] [--output-format text|json|json-pretty]\n\
         \x20 get <vmid> [<path>] [--comments] [--output-format text|json|json-pretty]\n\
         \x20 set <vmid> <k=v> [<k=v>...]\n\
         \x20 patch <vmid> '<json-patch>'\n\
         \x20 delete <vmid> <path>\n\
         \x20 raw <vmid>\n\
         \x20 edit <vmid>\n\
         \x20 convert <vmid> <yaml|toml|json>\n\
         \x20 remove <vmid>\n\
         \x20 destroy <vmid>\n\
         \x20 snapshot <vmid> <name>\n\
         \x20 rollback <vmid> <name>\n\
         \x20 delsnap <vmid> <name>\n\
         \x20 clone <vmid> <newid>\n\
         \x20 version\n\
         \x20 health\n\
         \x20 inventory\n\
         \x20 registry\n\
         \x20 schemas <vmid>\n\
         \x20 datacenter get|set|patch|raw|edit|convert [args...]"
    );
    std::process::exit(2);
}

fn take_flag(args: &mut Vec<String>, name: &str) -> bool {
    if let Some(pos) = args.iter().position(|a| a == name) {
        args.remove(pos);
        true
    } else {
        false
    }
}

fn take_opt(args: &mut Vec<String>, name: &str) -> Option<String> {
    let pos = args.iter().position(|a| a == name)?;
    args.remove(pos);
    if pos < args.len() {
        Some(args.remove(pos))
    } else {
        None
    }
}

fn parse_vmid(s: &str) -> Result<u32, Error> {
    s.parse().with_context(|| format!("invalid vmid '{s}'"))
}

/// Parses a CLI scalar the way `pve-meta set`/`patch` values are documented: valid JSON (number,
/// bool, array, object, quoted string) parses as JSON; anything else is a plain string.
fn parse_cli_value(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

/// Builds `{a:{b:{c: value}}}` for a dotted path `a.b.c`.
fn nest(path: &DocPath, value: Value) -> Value {
    let mut out = value;
    for seg in path.segments().iter().rev() {
        let mut map = serde_json::Map::new();
        map.insert(seg.clone(), out);
        out = Value::Object(map);
    }
    out
}

/// Merges `b` into `a` in place (plain recursive object merge; good enough to combine several
/// `set`/`delete` path assignments into one patch document).
fn merge_into(a: &mut Value, b: Value) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(existing) => merge_into(existing, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (a, b) => *a = b,
    }
}

fn build_patch_from_pairs(pairs: &[String]) -> Result<Value, Error> {
    let mut patch = Value::Object(serde_json::Map::new());
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected k=v, got '{pair}'"))?;
        let path = DocPath::parse(key).map_err(Error::from)?;
        merge_into(&mut patch, nest(&path, parse_cli_value(value)));
    }
    Ok(patch)
}

fn print_result(value: &Value, format: &str) {
    match format {
        "json" => println!("{value}"),
        "text" => print_text(value),
        _ => println!("{}", serde_json::to_string_pretty(value).unwrap()),
    }
}

/// A deliberately simple text renderer: an array of objects becomes a whitespace-aligned table;
/// anything else falls back to pretty JSON.
fn print_text(value: &Value) {
    let Value::Array(rows) = value else {
        println!("{}", serde_json::to_string_pretty(value).unwrap());
        return;
    };
    if rows.is_empty() {
        return;
    }
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(map) = row {
            for k in map.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    let cell = |v: &Value| -> String {
        match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        }
    };
    let mut widths: Vec<usize> = columns.iter().map(|c| c.len()).collect();
    let rendered: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|c| row.get(c).map(cell).unwrap_or_default())
                .collect()
        })
        .collect();
    for row in &rendered {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let header: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{c:width$}", width = widths[i]))
        .collect();
    println!("{}", header.join("  ").trim_end());
    for row in &rendered {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| format!("{cell:width$}", width = widths[i]))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

fn run_get(id: DocId, mut args: Vec<String>, default_format: &str) -> Result<(), Error> {
    let comments = take_flag(&mut args, "--comments");
    let output_format = take_opt(&mut args, "--output-format").unwrap_or_else(|| default_format.to_string());
    let path = args.first().cloned();
    let value = match (id, path) {
        (DocId::Guest(vmid), Some(p)) => guests::get_guest_subtree(vmid, p)?,
        (DocId::Guest(vmid), None) => guests::get_guest(vmid, comments, false)?,
        (DocId::Datacenter, Some(p)) => datacenter::get_datacenter_subtree(p)?,
        (DocId::Datacenter, None) => datacenter::get_datacenter(comments, false)?,
    };
    print_result(&value, &output_format);
    Ok(())
}

fn run_set(id: DocId, args: &[String]) -> Result<(), Error> {
    if args.is_empty() {
        bail!("set requires at least one k=v pair");
    }
    let patch = build_patch_from_pairs(args)?;
    let mut env = cli_env();
    let result = match id {
        DocId::Guest(vmid) => guests::patch_guest(vmid, patch, None, false, &mut env)?,
        DocId::Datacenter => datacenter::patch_datacenter(patch, None, false, &mut env)?,
    };
    print_result(&result, "json-pretty");
    Ok(())
}

fn run_patch(id: DocId, json: &str) -> Result<(), Error> {
    let patch: Value = serde_json::from_str(json).context("patch argument is not valid JSON")?;
    let mut env = cli_env();
    let result = match id {
        DocId::Guest(vmid) => guests::patch_guest(vmid, patch, None, false, &mut env)?,
        DocId::Datacenter => datacenter::patch_datacenter(patch, None, false, &mut env)?,
    };
    print_result(&result, "json-pretty");
    Ok(())
}

fn run_delete_path(id: DocId, path: &str) -> Result<(), Error> {
    let parsed = DocPath::parse(path)?;
    if parsed.is_root() {
        bail!("path must not be empty");
    }
    let patch = nest(&parsed, Value::Null);
    let mut env = cli_env();
    let result = match id {
        DocId::Guest(vmid) => guests::patch_guest(vmid, patch, None, false, &mut env)?,
        DocId::Datacenter => datacenter::patch_datacenter(patch, None, false, &mut env)?,
    };
    print_result(&result, "json-pretty");
    Ok(())
}

fn run_raw(id: DocId) -> Result<(), Error> {
    let doc = pve_meta_api::store::store().read(id)?;
    print!("{}", doc.raw);
    Ok(())
}

fn run_edit(id: DocId) -> Result<(), Error> {
    let (original, digest) = match pve_meta_api::store::store().read(id) {
        Ok(doc) => (doc.raw, Some(doc.digest)),
        Err(pve_meta_core::Error::NotFound(_)) => {
            let fmt = pve_meta_api::store::store().default_format()?;
            (pve_meta_core::format::dump(fmt, &Value::Object(Default::default())), None)
        }
        Err(e) => return Err(e.into()),
    };

    let tmp = tempfile::Builder::new().suffix(".pve-meta").tempfile()?;
    std::fs::write(tmp.path(), &original)?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(tmp.path())
        .status()
        .with_context(|| format!("launching editor '{editor}'"))?;
    if !status.success() {
        bail!("editor exited with a non-zero status; aborting");
    }

    let edited = std::fs::read_to_string(tmp.path())?;
    if edited == original {
        println!("No changes.");
        return Ok(());
    }

    let mut env = cli_env();
    let result = match id {
        DocId::Guest(vmid) => guests::put_guest_raw(vmid, edited, None, digest, false, &mut env)?,
        DocId::Datacenter => datacenter::put_datacenter_raw(edited, None, digest, false, &mut env)?,
    };
    print_result(&result, "json-pretty");
    Ok(())
}

fn run_convert(id: DocId, format: &str) -> Result<(), Error> {
    let mut env = cli_env();
    let result = match id {
        DocId::Guest(vmid) => guests::convert_guest(vmid, format.to_string(), None, &mut env)?,
        DocId::Datacenter => datacenter::convert_datacenter(format.to_string(), None, &mut env)?,
    };
    print_result(&result, "json-pretty");
    Ok(())
}

fn run_remove(id: DocId) -> Result<(), Error> {
    let mut env = cli_env();
    match id {
        DocId::Guest(vmid) => {
            guests::delete_guest(vmid, &mut env)?;
        }
        DocId::Datacenter => {
            datacenter::delete_datacenter(&mut env)?;
        }
    }
    Ok(())
}

fn dispatch_document_commands(id: DocId, cmd: &str, mut args: Vec<String>) -> Result<bool, Error> {
    match cmd {
        "get" => run_get(id, args, "json-pretty")?,
        "set" => run_set(id, &args)?,
        "patch" => {
            if args.is_empty() {
                bail!("patch requires a JSON argument");
            }
            run_patch(id, &args.remove(0))?;
        }
        "delete" => {
            if args.is_empty() {
                bail!("delete requires a path argument");
            }
            run_delete_path(id, &args.remove(0))?;
        }
        "raw" => run_raw(id)?,
        "edit" => run_edit(id)?,
        "convert" => {
            if args.is_empty() {
                bail!("convert requires a format argument");
            }
            run_convert(id, &args.remove(0))?;
        }
        "remove" | "destroy" => run_remove(id)?,
        _ => return Ok(false),
    }
    Ok(true)
}

fn main() -> Result<(), Error> {
    let _ = proxmox_log::Logger::from_env("PVE_META_LOG", proxmox_log::LevelFilter::WARN)
        .stderr()
        .init();
    pve_meta_api::store::init_default();

    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    let cmd = args.remove(0);

    match cmd.as_str() {
        "list" => {
            let has = take_opt(&mut args, "--has");
            let output_format = take_opt(&mut args, "--output-format").unwrap_or_else(|| "text".to_string());
            let result = guests::list_guests(has)?;
            print_result(&result, &output_format);
        }
        "version" => {
            let result = tokio::runtime::Runtime::new()?.block_on(meta::version(None, None))?;
            print_result(&result, "json-pretty");
        }
        "health" => print_result(&meta::health()?, "json-pretty"),
        "inventory" => {
            let mut env = cli_env();
            print_result(&meta::inventory(&mut env)?, "text");
        }
        "registry" => print_result(&registry::registry()?, "json-pretty"),
        "schemas" => {
            if args.is_empty() {
                bail!("schemas requires a vmid argument");
            }
            let vmid = parse_vmid(&args.remove(0))?;
            print_result(&registry::schemas_for_guest(vmid)?, "json-pretty");
        }
        "snapshot" | "rollback" | "delsnap" | "clone" => {
            if args.len() < 2 {
                bail!("{cmd} requires <vmid> <name-or-newid>");
            }
            let vmid = parse_vmid(&args[0])?;
            let mut env = cli_env();
            match cmd.as_str() {
                "snapshot" => {
                    let r = guests::snapshot_guest(vmid, args[1].clone(), &mut env)?;
                    print_result(&r, "json-pretty");
                }
                "rollback" => {
                    let r = guests::rollback_guest(vmid, args[1].clone(), &mut env)?;
                    print_result(&r, "json-pretty");
                }
                "delsnap" => {
                    guests::delete_snapshot(vmid, args[1].clone(), &mut env)?;
                }
                "clone" => {
                    let newid = parse_vmid(&args[1])?;
                    let r = guests::clone_guest(vmid, newid, &mut env)?;
                    print_result(&r, "json-pretty");
                }
                _ => unreachable!(),
            }
        }
        "datacenter" => {
            if args.is_empty() {
                usage();
            }
            let sub = args.remove(0);
            if !dispatch_document_commands(DocId::Datacenter, &sub, args)? {
                usage();
            }
        }
        "get" | "set" | "patch" | "delete" | "raw" | "edit" | "convert" | "remove" | "destroy" => {
            if args.is_empty() {
                bail!("{cmd} requires a <vmid> argument");
            }
            let vmid = parse_vmid(&args.remove(0))?;
            dispatch_document_commands(DocId::Guest(vmid), &cmd, args)?;
        }
        _ => usage(),
    }

    Ok(())
}
