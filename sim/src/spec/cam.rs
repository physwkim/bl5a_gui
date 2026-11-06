//! The mar345-ioc camera as the mar345 macros reach it: `epics_put` and
//! `epics_get` on the `BL5A:MAR345:cam1:` fields they name, over Channel
//! Access with the client's `EPICS_CA_*` environment.

use std::collections::HashMap;
use std::time::Duration;

use epics_ca_rs::client::{CaChannel, CaClient};
use epics_ca_rs::{DbFieldType, EpicsValue};

const CAM: &str = "BL5A:MAR345:cam1:";
const TIMEOUT: Duration = Duration::from_secs(3);

/// Connected on first use, so a session without mar macros needs no IOC.
#[derive(Default)]
pub struct Cam {
    client: Option<CaClient>,
    channels: HashMap<&'static str, CaChannel>,
}

impl Cam {
    async fn channel(&mut self, field: &'static str) -> Result<&CaChannel, String> {
        let client = match &mut self.client {
            Some(c) => c,
            None => self.client.insert(
                CaClient::new()
                    .await
                    .map_err(|e| format!("CA client: {e}"))?,
            ),
        };
        let ch = self
            .channels
            .entry(field)
            .or_insert_with(|| client.create_channel(format!("{CAM}{field}")));
        ch.wait_connected(TIMEOUT)
            .await
            .map_err(|e| format!("{CAM}{field}: {e}"))?;
        Ok(ch)
    }

    /// `epics_put` of a number, without waiting for completion: an
    /// `Acquire` put completes only when the scan does.
    pub async fn put_number(&mut self, field: &'static str, v: f64) -> Result<(), String> {
        let ch = self.channel(field).await?;
        ch.put_nowait(&EpicsValue::Double(v))
            .await
            .map_err(|e| format!("{CAM}{field}: {e}"))
    }

    /// `epics_put` of a string into a CHAR waveform: its bytes and the NUL.
    pub async fn put_text(&mut self, field: &'static str, text: &str) -> Result<(), String> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        let ch = self.channel(field).await?;
        ch.put_nowait(&EpicsValue::CharArray(bytes.into()))
            .await
            .map_err(|e| format!("{CAM}{field}: {e}"))
    }

    /// `epics_get` of an enum: its state string.
    pub async fn get_state(&mut self, field: &'static str) -> Result<String, String> {
        let ch = self.channel(field).await?;
        let snap = ch
            .get_with_dbr_type(DbFieldType::String as u16, 1u32, TIMEOUT)
            .await
            .map_err(|e| format!("{CAM}{field}: {e}"))?;
        match snap.value {
            EpicsValue::String(s) => Ok(s.to_string()),
            v => Err(format!("{CAM}{field}: {v:?} is not a string")),
        }
    }

    pub async fn get_number(&mut self, field: &'static str) -> Result<f64, String> {
        let ch = self.channel(field).await?;
        let (_, v) = ch
            .get_with_timeout(TIMEOUT)
            .await
            .map_err(|e| format!("{CAM}{field}: {e}"))?;
        v.to_f64()
            .ok_or_else(|| format!("{CAM}{field}: {v:?} is not a number"))
    }
}
