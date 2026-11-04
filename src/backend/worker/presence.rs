//! The account's own online presence. WhatsApp holds the phone's push
//! notifications while a linked device is online, so the window decides when
//! ZapTide is online (`Command::SetOnline`), delays included. Bots use
//! `PresencePolicy::Manual`, because whatsapp-rust would otherwise announce
//! "available" on its own on connect and on each push-name update.

use super::*;
use tokio::sync::watch;

/// The presence the window asks for, and a round that rises each time
/// WhatsApp needs it announced again: on connect and on a push-name update.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct PresenceWish {
    online: bool,
    round: u64,
}

impl Worker {
    /// The announcer applies this asynchronously and coalesces wishes made
    /// before it ran, so only the latest one is sent.
    pub(super) fn set_online(&mut self, online: bool) {
        self.presence_wish
            .send_if_modified(|wish| std::mem::replace(&mut wish.online, online) != online);
    }

    /// Announces the presence again; the server forgets it on reconnect and
    /// an announcement without a push name fails.
    pub(super) fn renew_presence(&mut self) {
        self.presence_wish.send_modify(|wish| wish.round += 1);
    }

    /// Announces this client's presence until the next bot replaces the sender.
    pub(super) fn spawn_presence(&mut self, client: Arc<Client>) {
        let (sender, wishes) = watch::channel(*self.presence_wish.borrow());
        self.presence_wish = sender;
        tokio::spawn(announce_presence(wishes, move |online| {
            let client = client.clone();
            async move {
                let presence = client.presence();
                let result = if online {
                    presence.set_available().await
                } else {
                    presence.set_unavailable().await
                };
                result
                    .inspect_err(|error| log::debug!("presence not announced: {error}"))
                    .is_ok()
            }
        }));
    }
}

/// Sends each wish once, in order; a failed announcement waits for the next round.
async fn announce_presence<F, Fut>(mut wishes: watch::Receiver<PresenceWish>, mut announce: F)
where
    F: FnMut(bool) -> Fut,
    Fut: Future<Output = bool>,
{
    let mut sent: Option<PresenceWish> = None;
    loop {
        let wish = *wishes.borrow_and_update();
        if sent != Some(wish) && announce(wish.online).await {
            sent = Some(wish);
        }
        if wishes.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn announcer(
        succeeds: bool,
    ) -> (
        watch::Sender<PresenceWish>,
        Arc<Mutex<Vec<bool>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let (sender, wishes) = watch::channel(PresenceWish::default());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let task = tokio::spawn(announce_presence(wishes, move |online| {
            log.lock().unwrap().push(online);
            std::future::ready(succeeds)
        }));
        (sender, sent, task)
    }

    #[tokio::test]
    async fn each_wish_is_sent_once_in_order() {
        let (wish, sent, _task) = announcer(true);
        tokio::task::yield_now().await;
        for online in [true, true, false] {
            wish.send_modify(|wish| wish.online = online);
            tokio::task::yield_now().await;
        }
        assert_eq!(*sent.lock().unwrap(), [false, true, false]);
    }

    #[tokio::test]
    async fn a_new_round_announces_again_and_failures_wait_for_it() {
        let (wish, sent, _task) = announcer(false);
        wish.send_modify(|wish| wish.online = true);
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            *sent.lock().unwrap(),
            [true],
            "a failure is not retried in a loop"
        );
        wish.send_modify(|wish| wish.round += 1);
        tokio::task::yield_now().await;
        assert_eq!(*sent.lock().unwrap(), [true, true]);
    }

    #[tokio::test]
    async fn the_announcer_stops_with_its_sender() {
        let (wish, _sent, task) = announcer(true);
        drop(wish);
        task.await.expect("the announcer ends");
    }
}
