use std::{
    fmt::{self, Formatter},
    path::PathBuf,
    sync::Arc,
};

use crate::{
    error::MokshaMintError,
    model::{CreateInvoiceResult, PayInvoiceResult},
    url_serialize::{deserialize_url, serialize_url},
};
use async_trait::async_trait;
use fedimint_tonic_lnd::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard};
use url::Url;

use super::Lightning;

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct LndLightningSettings {
    #[serde(serialize_with = "serialize_url", deserialize_with = "deserialize_url")]
    pub grpc_host: Option<Url>,
    pub tls_cert_path: Option<PathBuf>,
    pub macaroon_path: Option<PathBuf>,
}
impl fmt::Display for LndLightningSettings {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "grpc_host: {}, tls_cert_path: {}, macaroon_path: {}",
            self.grpc_host.as_ref().unwrap(),
            self.tls_cert_path
                .as_ref()
                .unwrap() // FIXME unwrap
                .to_str()
                .unwrap_or_default(),
            self.macaroon_path
                .as_ref()
                .unwrap()
                .to_str()
                .unwrap_or_default()
        )
    }
}

pub struct LndLightning(Arc<Mutex<Client>>);

impl LndLightning {
    pub async fn new(
        address: Url,
        cert_file: &PathBuf,
        macaroon_file: &PathBuf,
    ) -> Result<Self, MokshaMintError> {
        let client =
            fedimint_tonic_lnd::connect(address.to_string(), cert_file, &macaroon_file).await;

        Ok(Self(Arc::new(Mutex::new(
            client.map_err(MokshaMintError::ConnectError)?,
        ))))
    }

    pub async fn client_lock(
        &self,
    ) -> anyhow::Result<MappedMutexGuard<'_, fedimint_tonic_lnd::LightningClient>> {
        let guard = self.0.lock().await;
        Ok(MutexGuard::map(guard, |client| client.lightning()))
    }
}

#[allow(implied_bounds_entailment)]
#[async_trait]
impl Lightning for LndLightning {
    async fn is_invoice_paid(&self, payment_request: String) -> Result<bool, MokshaMintError> {
        let invoice = self.decode_invoice(payment_request).await?;
        let payment_hash = invoice.payment_hash();
        let invoice_request = fedimint_tonic_lnd::lnrpc::PaymentHash {
            r_hash: payment_hash.to_vec(),
            ..Default::default()
        };

        let invoice = self
            .client_lock()
            .await
            .expect("failed to lock client")
            .lookup_invoice(fedimint_tonic_lnd::tonic::Request::new(invoice_request))
            .await
            .expect("failed to lookup invoice")
            .into_inner();

        Ok(invoice.state == fedimint_tonic_lnd::lnrpc::invoice::InvoiceState::Settled as i32)
    }

    async fn create_invoice(&self, amount: u64) -> Result<CreateInvoiceResult, MokshaMintError> {
        let invoice_request = fedimint_tonic_lnd::lnrpc::Invoice {
            value: amount as i64,
            ..Default::default()
        };

        let invoice = self
            .client_lock()
            .await
            .expect("failed to lock client")
            .add_invoice(fedimint_tonic_lnd::tonic::Request::new(invoice_request))
            .await
            .expect("failed to create invoice")
            .into_inner();

        Ok(CreateInvoiceResult {
            payment_hash: invoice.r_hash,
            payment_request: invoice.payment_request,
        })
    }

    async fn pay_invoice(
        &self,
        payment_request: String,
    ) -> Result<PayInvoiceResult, MokshaMintError> {
        let pay_req = fedimint_tonic_lnd::lnrpc::SendRequest {
            payment_request,
            ..Default::default()
        };
        let payment_response = self
            .client_lock()
            .await
            .expect("failed to lock client") //FIXME map error
            .send_payment_sync(fedimint_tonic_lnd::tonic::Request::new(pay_req))
            .await
            .expect("failed to pay invoice")
            .into_inner();

        let total_fees = payment_response
            .payment_route
            .map_or(0, |route| route.total_fees_msat) as u64;

        Ok(PayInvoiceResult {
            payment_hash: hex::encode(payment_response.payment_hash),
            total_fees,
        })
    }
}
