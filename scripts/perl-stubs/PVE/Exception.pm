package PVE::Exception;
use strict; use warnings;
use Exporter qw(import);
our @EXPORT_OK = qw(raise raise_param_exc raise_perm_exc);
sub raise { }
sub raise_param_exc { }
sub raise_perm_exc { }
1;
