use strict;
use warnings;
use Test::More tests => 5;

ok(1, 'passes');
is(1 + 1, 3, 'fails an assertion');
SKIP: {
    skip 'not on this platform', 1;
    ok(1, 'skipped');
}
TODO: {
    local $TODO = 'not implemented yet';
    ok(0, 'todo fails');
}
ok(0);
