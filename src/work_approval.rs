//! Bind external approvals to a Work revision at the host mutation boundary.
use crate::work_model::{
    DurableTransitionMetadata, DurableTransitionOrigin, ManagedWorkItemFile, WorkItem,
    WorkModelError, WorkModelStore,
};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Clone)]
pub struct WorkApproval {
    pub operation: String,
    pub expected_state: String,
}
impl WorkApproval {
    pub fn fingerprint(item: &WorkItem) -> String {
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(item).expect("Work serializes"))
        )
    }
    pub fn check(&self, item: &WorkItem) -> Result<(), WorkModelError> {
        let pristine_creation = self.expected_state == "new"
            && item.planning_context.is_none()
            && item.attempts.is_empty()
            && item.instructions.is_none()
            && item.abandonment.is_none()
            && item.authorization.is_proposed()
            && item.storage_revision.get() == Some(0);
        if !pristine_creation && Self::fingerprint(item) != self.expected_state {
            return Err(WorkModelError::StaleExternalApproval);
        }
        Ok(())
    }
    fn receipt(&self, id: &str) -> ManagedWorkItemFile {
        let digest = format!("{:x}", Sha256::digest(self.operation.as_bytes()));
        ManagedWorkItemFile::new(
            format!(".fluent/work/artifacts/{id}/external-approvals/{digest}.json"),
            serde_json::to_vec(&(&self.operation, &self.expected_state))
                .expect("approval serializes"),
        )
    }
    pub fn applied(&self, checkout: &Path, id: &str) -> Result<bool> {
        let receipt = self.receipt(id);
        match std::fs::read(checkout.join(receipt.path)) {
            Ok(bytes) => {
                anyhow::ensure!(bytes == receipt.bytes, "approval receipt changed");
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    /// Record admission under the model lock. This receipt never proves completion
    /// of an external effect, and every retry must still match the approved state.
    pub fn admit(&self, checkout: &Path, id: &str) -> Result<()> {
        let mut admission = self.clone();
        admission.operation.push_str(":admission");
        admission.mutate_inner(checkout, id, false, |_| Ok(()))
    }
    /// The effect and immutable receipt publish in one recoverable Work transaction.
    pub fn mutate(
        &self,
        checkout: &Path,
        id: &str,
        reducer: impl FnOnce(&mut WorkItem) -> Result<(), WorkModelError>,
    ) -> Result<()> {
        self.mutate_inner(checkout, id, true, reducer)
    }
    fn mutate_inner(
        &self,
        checkout: &Path,
        id: &str,
        reconcile: bool,
        reducer: impl FnOnce(&mut WorkItem) -> Result<(), WorkModelError>,
    ) -> Result<()> {
        let store = WorkModelStore::new(checkout);
        let receipt = self.receipt(id);
        store.mutate_work_item_with_transition_and_files(
            id,
            &DurableTransitionMetadata {
                origin: DurableTransitionOrigin::External,
                reason: Some("Apply approved Work revision".into()),
                evidence_ref: Some(receipt.path.clone()),
            },
            &[receipt],
            |item| {
                // Transaction recovery precedes this reducer under model.lock. The receipt
                // therefore proves the effect, even if later revisions have superseded it.
                if reconcile
                    && self
                        .applied(checkout, id)
                        .map_err(|_| WorkModelError::StaleExternalApproval)?
                {
                    return Ok(());
                }
                self.check(item)?;
                reducer(item)
            },
        )?;
        Ok(())
    }
}
