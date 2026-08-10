//! Фоновая задача-«уборщик» протухших ссылок.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use domain::LinkRepository;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Запускает уборщика в `tracker`; задача завершится по `token.cancel()`.
pub fn spawn_cleaner(
    repo: Arc<dyn LinkRepository>,
    period: Duration,
    tracker: &TaskTracker,
    token: CancellationToken,
) {
    tracker.spawn(run_cleaner(repo, period, token));
}

async fn run_cleaner(repo: Arc<dyn LinkRepository>, period: Duration, token: CancellationToken) {
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                tracing::info!("cleaner: cancellation requested, exiting");
                return;
            }
            _ = interval.tick() => {
                let removed = repo.purge_expired(SystemTime::now()).await;
                if removed > 0 {
                    tracing::info!(removed, "cleaner: purged expired links");
                } else {
                    tracing::debug!("cleaner: nothing to purge");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use domain::{LinkRepository, ShortLink};
    use storage::InMemoryRepo;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cleaner_purges_expired_links() {
        let repo = Arc::new(InMemoryRepo::new());

        // Используем асинхронные методы через трейт
        repo.insert(
            ShortLink::new("old", "https://example.com/old")
                .with_expires_at(SystemTime::now() - Duration::from_secs(3600)),
        )
        .await
        .unwrap();

        repo.insert(ShortLink::new("fresh", "https://example.com/fresh"))
            .await
            .unwrap();

        let token = CancellationToken::new();
        let tracker = TaskTracker::new();
        spawn_cleaner(
            repo.clone(),
            Duration::from_secs(30),
            &tracker,
            token.clone(),
        );

        tokio::time::sleep(Duration::from_secs(31)).await;

        // Асинхронная проверка
        let old_result = repo.get("old").await;
        assert!(old_result.is_err(), "expired link must be purged");

        let fresh_result = repo.get("fresh").await;
        assert!(fresh_result.is_ok(), "live link must survive");

        token.cancel();
        tracker.close();
        tokio::time::timeout(Duration::from_secs(5), tracker.wait())
            .await
            .expect("cleaner must exit promptly after cancel");
    }
}
