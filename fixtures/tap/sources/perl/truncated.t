use strict;
use warnings;
use Test::More;

ok(1, 'passes');
ok(0, 'fails before the crash');
die "crashed mid-run\n";
