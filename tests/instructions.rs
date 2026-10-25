use shellboy::{config::Config, instructions::InstructionLoader};

#[tokio::test]
async fn instructions_change_only_on_the_next_prepared_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("AGENTS.md");
    let loader = InstructionLoader::new(&Config::default());
    assert!(
        loader
            .prepare(directory.path().into(), path.clone())
            .await
            .unwrap()
            .is_none()
    );
    std::fs::write(&path, "First turn 日本語").unwrap();
    let first = loader
        .prepare(directory.path().into(), path.clone())
        .await
        .unwrap()
        .unwrap();
    let same = loader
        .prepare(directory.path().into(), path.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.body, same.body);
    std::fs::write(&path, "Changed instructions for next turn").unwrap();
    let next = loader
        .prepare(directory.path().into(), path.clone())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(first.hash, next.hash);
    assert_eq!(&*first.body, "First turn 日本語");
    std::fs::remove_file(&path).unwrap();
    assert!(
        loader
            .prepare(directory.path().into(), path.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        loader
            .prepare(directory.path().join("missing"), path.clone())
            .await
            .is_err()
    );
    std::fs::create_dir(&path).unwrap();
    assert!(loader.prepare(directory.path().into(), path).await.is_err());
}

#[tokio::test]
async fn invalid_utf8_is_not_treated_as_absent_or_served_from_old_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("AGENTS.md");
    let loader = InstructionLoader::new(&Config::default());
    std::fs::write(&path, "old").unwrap();
    loader
        .prepare(directory.path().into(), path.clone())
        .await
        .unwrap();
    std::fs::write(&path, [0xff, 0xfe, 0xfd, 0xfc]).unwrap();
    assert!(loader.prepare(directory.path().into(), path).await.is_err());
}

#[tokio::test]
async fn instruction_loader_does_not_retain_snapshots_or_use_the_replay_budget() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.cache.history_bytes = 4096;
    let loader = InstructionLoader::new(&config);
    let one = root.path().join("one.md");
    let two = root.path().join("two.md");
    std::fs::write(&one, "a".repeat(2048)).unwrap();
    std::fs::write(&two, "b".repeat(2048)).unwrap();
    let first = loader
        .prepare(root.path().into(), one.clone())
        .await
        .unwrap()
        .unwrap();
    loader
        .prepare(root.path().into(), two)
        .await
        .unwrap()
        .unwrap();
    let reread = loader
        .prepare(root.path().into(), one.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first.body, &reread.body));
    assert_eq!(&*first.body, "a".repeat(2048));
    let repeated = loader
        .prepare(root.path().into(), one.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&reread.body, &repeated.body));
    let weak = std::sync::Arc::downgrade(&repeated.body);
    drop(repeated);
    assert!(
        weak.upgrade().is_none(),
        "the loader keeps no extra RAM cache"
    );
    config.cache.history_bytes = 0;
    let uncached = InstructionLoader::new(&config);
    let first = uncached
        .prepare(root.path().into(), one.clone())
        .await
        .unwrap()
        .unwrap();
    let next = uncached
        .prepare(root.path().into(), one)
        .await
        .unwrap()
        .unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first.body, &next.body));
    loader.wait_idle().await.unwrap();
    uncached.wait_idle().await.unwrap();
}

#[tokio::test]
async fn equal_size_and_modified_time_do_not_hide_changed_turn_instructions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("AGENTS.md");
    let loader = InstructionLoader::new(&Config::default());
    std::fs::write(&path, "first").unwrap();
    let observed = std::fs::metadata(&path).unwrap().modified().unwrap();
    let first = loader
        .prepare(root.path().into(), path.clone())
        .await
        .unwrap()
        .unwrap();
    std::fs::write(&path, "other").unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(observed))
        .unwrap();
    let second = loader
        .prepare(root.path().into(), path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.modified, second.modified);
    assert_eq!(first.size, second.size);
    assert_ne!(first.hash, second.hash);
    assert_eq!(&*first.body, "first");
    assert_eq!(&*second.body, "other");
}
