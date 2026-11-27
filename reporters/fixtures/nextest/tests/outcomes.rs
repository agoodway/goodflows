use std::{fs, path::PathBuf};

#[test]
fn passes() {
    assert_eq!(1 + 1, 2);
}

#[test]
fn fails_on_purpose() {
    assert_eq!("private assertion value", "another value");
}

#[test]
#[ignore]
fn is_ignored() {
    panic!("never runs");
}

// Fails on its first attempt and passes on the retry nextest gives it.
#[test]
fn flaky() {
    let marker = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "flaky-{}",
        std::env::var("NEXTEST_RUN_ID").unwrap_or_default()
    ));
    if fs::create_dir(&marker).is_ok() {
        panic!("first attempt fails");
    }
}

#[test]
fn aborts() {
    std::process::abort();
}

#[test]
fn times_out() {
    std::thread::sleep(std::time::Duration::from_secs(30));
}

mod math {
    mod nested {
        #[test]
        fn multiplies() {
            assert_eq!(2 * 3, 6);
        }
    }
}
