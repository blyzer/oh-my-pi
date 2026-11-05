#[test]
fn passes() { assert_eq!(1 + 1, 2); }

#[test]
fn fails() { assert_eq!(1 + 1, 3, "intentional failure"); }

#[test]
#[ignore = "intentional skip"]
fn skipped() {}

#[test]
fn errors() { panic!("intentional error"); }
