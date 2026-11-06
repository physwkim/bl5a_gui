//! Plans as async functions. A body talks to the engine through [`Ctx`]
//! and sees every result inline; each message goes out as a `Respond`
//! item, which the engine never caches for a rewind, so a pause fails the
//! plan on resume instead of replaying half a SPEC macro.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_stream::stream;
use bsrs::core::msg::{
    AwaitableFactory, LocatableObj, MovableObj, MsgResult, ReadableObj, TriggerableObj,
};
use bsrs::core::{BsrsError, Msg, Plan, PlanItem, ReadingValue, plan_items, respond};
use futures::{FutureExt, StreamExt};
use tokio::sync::{mpsc, oneshot};

/// A plan step's outcome; the error becomes the plan's `Msg::Fail`.
pub type Step<T = ()> = std::result::Result<T, String>;

/// An item, and the signal that the engine asked for the next one, i.e.
/// handled this one.
type Envelope = (PlanItem, oneshot::Sender<()>);

const INTERRUPTED: &str = "plan interrupted: a paused plan cannot resume, abort it";

/// The most a [`Ctx::pausable_sleep`] runs between its checkpoints.
const TICK: Duration = Duration::from_secs(1);

fn interrupted<E>(_: E) -> String {
    INTERRUPTED.to_string()
}

/// The engine as a plan body sees it.
pub struct Ctx {
    tx: mpsc::Sender<Envelope>,
}

/// The plan `f` drives. A body that fails a message is dropped where it
/// awaits (the enclosing wrappers stop polling it); one that returns
/// `Err` fails the plan with that text.
pub fn plan_fn<F, Fut>(f: F) -> Plan
where
    F: FnOnce(Ctx) -> Fut + Send + 'static,
    Fut: Future<Output = Step> + Send + 'static,
{
    plan_items(stream! {
        let (tx, mut rx) = mpsc::channel::<Envelope>(1);
        let body = f(Ctx { tx });
        tokio::pin!(body);
        let result = loop {
            let next = tokio::select! {
                biased;
                Some(envelope) = rx.recv() => Ok(envelope),
                r = &mut body => Err(r),
            };
            match next {
                Ok((item, handled)) => {
                    yield item;
                    let _ = handled.send(());
                }
                Err(r) => break r,
            }
        };
        if let Err(e) = result {
            yield PlanItem::from(Msg::Fail(e));
        }
    })
}

impl Ctx {
    /// The engine's result for `msg`.
    pub async fn send(&self, msg: Msg) -> Step<MsgResult> {
        let (item, result) = respond(msg);
        let (handled, _) = oneshot::channel();
        self.tx.send((item, handled)).await.map_err(interrupted)?;
        result.await.map_err(interrupted)
    }

    /// Every item of `plan`, each handed on only once the engine handled
    /// the one before, so a wrapper inside `plan` reads its contingency
    /// sink before it is polled again.
    pub async fn run(&self, mut plan: Plan) -> Step {
        while let Some(item) = plan.next().await {
            let (handled, done) = oneshot::channel();
            self.tx.send((item, handled)).await.map_err(interrupted)?;
            done.await.map_err(interrupted)?;
        }
        Ok(())
    }

    /// `fut`, run by the engine as a `WaitFor`: an abort cancels it, and
    /// its error fails the plan.
    pub async fn call<T, F>(&self, fut: F) -> Step<T>
    where
        T: Send + 'static,
        F: Future<Output = bsrs::core::Result<T>> + Send + 'static,
    {
        let slot = Arc::new(Mutex::new(None));
        let out = slot.clone();
        let once = Mutex::new(Some(
            async move {
                let v = fut.await?;
                *out.lock().unwrap() = Some(v);
                Ok(())
            }
            .boxed(),
        ));
        let factory: AwaitableFactory = Arc::new(move || {
            once.lock().unwrap().take().unwrap_or_else(|| {
                async { Err(BsrsError::Plan("Ctx::call awaited twice".into())) }.boxed()
            })
        });
        self.send(Msg::WaitFor {
            factories: vec![factory],
            timeout: None,
        })
        .await?;
        let v = slot.lock().unwrap().take();
        v.ok_or_else(|| INTERRUPTED.to_string())
    }

    pub async fn set(&self, obj: Arc<dyn MovableObj>, value: f64, group: &str) -> Step {
        let msg = Msg::Set {
            obj,
            value,
            group: Some(group.into()),
        };
        self.send(msg).await.map(drop)
    }

    pub async fn trigger(&self, obj: Arc<dyn TriggerableObj>, group: &str) -> Step {
        let msg = Msg::Trigger {
            obj,
            group: Some(group.into()),
        };
        self.send(msg).await.map(drop)
    }

    /// Until every status in `group` is done; a failed one fails the plan.
    pub async fn wait(&self, group: &str) -> Step {
        let msg = Msg::Wait {
            group: group.into(),
            error_on_timeout: false,
            timeout: None,
        };
        match self.send(msg).await? {
            MsgResult::WaitComplete { .. } => Ok(()),
            other => Err(format!("wait {group}: {other:?}")),
        }
    }

    /// SPEC `umv`: every move at once, then until all are done.
    pub async fn mv(&self, moves: &[(Arc<dyn MovableObj>, f64)]) -> Step {
        for (obj, value) in moves {
            self.set(obj.clone(), *value, "mv").await?;
        }
        self.wait("mv").await
    }

    pub async fn sleep(&self, d: Duration) -> Step {
        self.send(Msg::Sleep(d)).await.map(drop)
    }

    /// `d`, as checkpoints at most a [`TICK`] apart, the first at once and
    /// the last at the end: a deferred pause takes effect within a `TICK`,
    /// which a `Msg::Sleep` would hold back until it ends.
    pub async fn pausable_sleep(&self, d: Duration) -> Step {
        let end = Instant::now() + d;
        loop {
            self.send(Msg::Checkpoint).await?;
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(());
            }
            self.sleep(left.min(TICK)).await?;
        }
    }

    pub async fn create(&self, stream: &str) -> Step {
        let msg = Msg::Create {
            stream_name: stream.into(),
        };
        self.send(msg).await.map(drop)
    }

    pub async fn read(&self, obj: Arc<dyn ReadableObj>) -> Step<HashMap<String, ReadingValue>> {
        match self.send(Msg::Read(obj)).await? {
            MsgResult::Reading { data } => Ok(data),
            other => Err(format!("read: {other:?}")),
        }
    }

    pub async fn save(&self) -> Step {
        self.send(Msg::Save).await.map(drop)
    }

    /// The readback.
    pub async fn position(&self, obj: Arc<dyn LocatableObj>) -> Step<f64> {
        match self.send(Msg::Locate(obj)).await? {
            MsgResult::Location { readback, .. } => Ok(readback),
            other => Err(format!("locate: {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bsrs::engine::RunEngine;

    fn sleeping(d: Duration) -> Plan {
        plan_fn(move |ctx| async move { ctx.pausable_sleep(d).await })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deferred_pause_lands_within_a_tick() {
        let re = Arc::new(RunEngine::new(vec![]));
        let run = tokio::spawn({
            let re = re.clone();
            async move { re.run_async(sleeping(Duration::from_secs(60))).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        re.request_pause(true);
        while !re.is_paused() {
            assert!(asked.elapsed() < TICK + Duration::from_millis(300));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        re.stop();
        let ended = tokio::time::timeout(Duration::from_secs(5), run).await;
        assert!(ended.expect("stop ends the plan").unwrap().is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unpaused_it_ends_at_its_end() {
        let re = RunEngine::new(vec![]);
        let t = Instant::now();
        re.run_async(sleeping(Duration::from_millis(1500)))
            .await
            .unwrap();
        let took = t.elapsed();
        assert!(took >= Duration::from_millis(1500), "{took:?}");
        assert!(took < Duration::from_millis(1800), "{took:?}");
    }
}
