//! Owner-private admission. Neither credentials nor tickets are serializable.
use anyhow::{ensure, Context, Result};
use rmpv::Value;
use std::sync::{Arc, Mutex, MutexGuard};
use uuid::Uuid;

use crate::{store::State, wire::s};

pub const CAPABILITY: &str = "file-transfer-v1";

#[derive(Default, Clone)]
pub struct Admission(Arc<Mutex<Option<Credentials>>>);

struct Credentials {
    generation: Uuid,
    session: Uuid,
    client: Uuid,
    epoch: u64,
    broker: Vec<u8>,
    token: Vec<u8>,
    attachment: Vec<u8>,
    lease: Vec<u8>,
}

pub struct Ticket {
    admission: Admission,
    generation: Uuid,
}

pub struct CommitGuard<'a> {
    _credentials: MutexGuard<'a, Option<Credentials>>,
}

pub struct ConnectionScope(Admission);
impl Drop for ConnectionScope {
    fn drop(&mut self) {
        self.0.revoke();
    }
}

impl Admission {
    pub fn connection_scope(&self) -> ConnectionScope {
        self.revoke();
        ConnectionScope(self.clone())
    }

    pub fn revoke(&self) {
        *self.0.lock().unwrap() = None;
    }

    pub(crate) fn publish(&self, state: &State, attachment: &[u8], lease: &[u8]) -> Result<()> {
        ensure!(!state.ended && attachment.len() == 16 && lease.len() == 16,
            "invalid transfer attachment");
        let token = state.token.as_ref().context("no transfer authorization")?;
        let broker = state.origin.as_ref().context("no transfer broker")?;
        ensure!(token.len() == 32 && broker.len() == 16, "invalid transfer authorization");
        *self.0.lock().unwrap() = Some(Credentials {
            generation: Uuid::now_v7(), session: state.id, client: state.client_id,
            epoch: state.epoch, broker: broker.clone(), token: token.clone(),
            attachment: attachment.to_vec(), lease: lease.to_vec(),
        });
        Ok(())
    }

    pub fn ticket(&self) -> Result<Ticket> {
        let generation = self.0.lock().unwrap().as_ref()
            .context("no live file-transfer-capable attachment")?.generation;
        Ok(Ticket { admission: self.clone(), generation })
    }
}

impl Ticket {
    pub fn check(&self) -> Result<()> {
        self.commit_guard().map(drop)
    }

    pub fn commit_guard(&self) -> Result<CommitGuard<'_>> {
        let credentials = self.admission.0.lock().unwrap();
        ensure!(credentials.as_ref().is_some_and(|c| c.generation == self.generation),
            "transfer attachment revoked");
        Ok(CommitGuard { _credentials: credentials })
    }

    /// Only the remote protocol encoder receives these fields; never discovery/JSON.
    pub fn authorization(&self) -> Result<Vec<(&'static str, Value)>> {
        let guard = self.commit_guard()?;
        let c = guard._credentials.as_ref().context("transfer attachment revoked")?;
        Ok(vec![
            ("session_id", s(&c.session.to_string())),
            ("client_instance_id", Value::Binary(c.client.as_bytes().to_vec())),
            ("connection_epoch", c.epoch.into()),
            ("broker_instance_id", Value::Binary(c.broker.clone())),
            ("resume_token", Value::Binary(c.token.clone())),
            ("attachment_id", Value::Binary(c.attachment.clone())),
            ("lease_id", Value::Binary(c.lease.clone())),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> State {
        let mut state = State::new("fixture".into(), "powershell.exe".into(), vec![], None);
        state.token = Some(vec![1; 32]);
        state.origin = Some(vec![2; 16]);
        state
    }

    #[test]
    fn connection_loss_and_new_attachment_permanently_revoke_old_tickets() {
        let admission = Admission::default();
        assert!(admission.ticket().is_err());
        let scope = admission.connection_scope();
        admission.publish(&state(), &[3; 16], &[4; 16]).unwrap();
        let old = admission.ticket().unwrap();
        assert!(old.check().is_ok());
        admission.publish(&state(), &[5; 16], &[6; 16]).unwrap();
        assert!(old.check().is_err());
        let current = admission.ticket().unwrap();
        drop(scope);
        assert!(current.check().is_err());
        assert!(admission.ticket().is_err());
    }

    #[test]
    fn revocation_waits_for_atomic_commit_guard() {
        let admission = Admission::default();
        admission.publish(&state(), &[3; 16], &[4; 16]).unwrap();
        let ticket = admission.ticket().unwrap();
        let guard = ticket.commit_guard().unwrap();
        let revoke = admission.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            revoke.revoke();
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(done_rx.try_recv().is_err());
        drop(guard);
        done_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
        assert!(ticket.check().is_err());
    }
}
