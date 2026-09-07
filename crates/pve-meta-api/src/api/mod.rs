//! The `/api2/json/meta/...` router tree.

pub mod common;
pub mod datacenter;
pub mod guests;
pub mod meta;
pub mod registry;

use proxmox_router::{list_subdirs_api_method, Router, SubdirMap};

const META_SUBDIRS: SubdirMap = &[
    ("datacenter", &datacenter::DATACENTER_ROUTER),
    ("guests", &guests::GUESTS_ROUTER),
    ("health", &Router::new().get(&meta::API_METHOD_HEALTH)),
    ("inventory", &Router::new().get(&meta::API_METHOD_INVENTORY)),
    ("registry", &registry::REGISTRY_ROUTER),
    ("schemas", &registry::SCHEMAS_ROUTER),
    ("version", &Router::new().get(&meta::API_METHOD_VERSION)),
];

const META_ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(META_SUBDIRS))
    .subdirs(META_SUBDIRS);

const TOP_SUBDIRS: SubdirMap = &[("meta", &META_ROUTER)];

/// The whole `pve-metad` API tree, to be registered via
/// `ApiConfig::default_api2_handler(&pve_meta_api::api::ROUTER)`.
pub const ROUTER: Router = Router::new()
    .get(&list_subdirs_api_method!(TOP_SUBDIRS))
    .subdirs(TOP_SUBDIRS);
