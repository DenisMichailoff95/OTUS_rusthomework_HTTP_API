//! Stress-тесты на корректность конкурентного счётчика переходов.
//!
//! 8 потоков по 10 000 инкрементов в одну ссылку: у корректных
//! реализаций итог ровно 80 000. Намеренно сломанная версия
//! теряет обновления.

use domain::{LinkRepository, ShortLink};
use storage::{DashMapRepo, InMemoryRepo, InMemoryRepoV1, broken::LostUpdateRepo};

const THREADS: usize = 8;
const HITS_PER_THREAD: usize = 10_000;
const TOTAL: u64 = (THREADS * HITS_PER_THREAD) as u64;

fn hammer(hit: impl Fn() + Sync) {
    std::thread::scope(|s| {
        for _ in 0..THREADS {
            s.spawn(|| {
                for _ in 0..HITS_PER_THREAD {
                    hit();
                }
            });
        }
    });
}

/// Синхронные тесты для v1 (только синхронный API)
#[test]
fn v1_no_lost_updates() {
    let repo = InMemoryRepoV1::new();
    repo.insert(ShortLink::new("hot", "https://example.com/"))
        .unwrap();
    hammer(|| {
        repo.record_hit("hot").unwrap();
    });
    assert_eq!(repo.stats("hot").unwrap().hits, TOTAL);
}

/// Асинхронные тесты для v2 через трейт
#[tokio::test]
async fn v2_no_lost_updates_async() {
    let repo = InMemoryRepo::new();
    repo.insert(ShortLink::new("hot", "https://example.com/"))
        .await
        .unwrap();

    // Используем Arc для разделения между задачами
    let repo = std::sync::Arc::new(repo);
    let mut handles = Vec::new();

    for _ in 0..THREADS {
        let repo = repo.clone();
        let code = "hot".to_string();
        handles.push(tokio::spawn(async move {
            for _ in 0..HITS_PER_THREAD {
                // Явно указываем тип для трейта
                let _ = LinkRepository::record_hit(&*repo, &code).await.unwrap();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let stats = LinkRepository::stats(&*repo, "hot").await.unwrap();
    assert_eq!(stats.hits, TOTAL);
}

/// DashMap асинхронный тест
#[tokio::test]
async fn dashmap_no_lost_updates_async() {
    let repo = DashMapRepo::new();
    repo.insert(ShortLink::new("hot", "https://example.com/"))
        .await
        .unwrap();

    let repo = std::sync::Arc::new(repo);
    let mut handles = Vec::new();

    for _ in 0..THREADS {
        let repo = repo.clone();
        let code = "hot".to_string();
        handles.push(tokio::spawn(async move {
            for _ in 0..HITS_PER_THREAD {
                let _ = LinkRepository::record_hit(&*repo, &code).await.unwrap();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let stats = LinkRepository::stats(&*repo, "hot").await.unwrap();
    assert_eq!(stats.hits, TOTAL);
}

/// Тест сломанной версии (синхронный, для демонстрации)
#[test]
fn broken_repo_loses_updates() {
    let repo = LostUpdateRepo::new();
    repo.insert(ShortLink::new("hot", "https://example.com/"))
        .unwrap();
    hammer(|| {
        repo.record_hit("hot").unwrap();
    });
    let hits = repo.stats("hot").unwrap().hits;
    assert!(hits <= TOTAL, "counter overshoot is impossible: {hits}");
    println!(
        "broken repo: {hits} of {TOTAL} hits recorded, lost {}",
        TOTAL - hits
    );
}

/// Асинхронный конкурентный тест для InMemoryRepo с большим числом запросов
#[tokio::test(flavor = "multi_thread")]
async fn async_concurrent_heavy_load() {
    let repo = InMemoryRepo::new();
    repo.insert(ShortLink::new("heavy", "https://example.com/"))
        .await
        .unwrap();

    let repo = std::sync::Arc::new(repo);
    let tasks = 10;
    let hits_per_task = 100_000;
    let expected_total = (tasks * hits_per_task) as u64;

    let mut handles = Vec::new();
    for _ in 0..tasks {
        let repo = repo.clone();
        let code = "heavy".to_string();
        handles.push(tokio::spawn(async move {
            for _ in 0..hits_per_task {
                let _ = LinkRepository::record_hit(&*repo, &code).await.unwrap();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let stats = LinkRepository::stats(&*repo, "heavy").await.unwrap();
    assert_eq!(stats.hits, expected_total);
}
