//! Explicit row9 budget provenance; old layouts cannot acquire its meaning.

use super::*;

#[test]
fn active_metadata_budget_reference_roundtrip_and_legacy_fences() {
    use mez_agent::token_budget::{BudgetCheckpointReference, BudgetReceiptId, BudgetRootId};
    let mut record = agent_session_metadata("$budget", "conversation");
    record.budget_reference = Some(BudgetCheckpointReference {
        namespace: BudgetReceiptId::new("namespace").unwrap(),
        root: BudgetRootId::new("root").unwrap(),
        conversation: "conversation".into(),
        operation: BudgetReceiptId::new("operation").unwrap(),
        owner: BudgetReceiptId::new("owner").unwrap(),
    });
    let encoded = encode_agent_session_metadata(&record).unwrap();
    assert_eq!(decode_agent_session_metadata(&encoded).unwrap(), record);
    let fields: Vec<_> = encoded.split('\t').collect();
    assert_eq!(fields.len(), 38);
    let legacy = fields[..37]
        .join("\t")
        .replacen("metadata/9", "metadata/8", 1);
    let mut unknown = record.clone();
    unknown.budget_reference = None;
    assert_eq!(decode_agent_session_metadata(&legacy).unwrap(), unknown);
    assert!(
        decode_agent_session_metadata(&encoded.replacen("metadata/9", "metadata/8", 1)).is_err()
    );
    assert!(decode_agent_session_metadata(&fields[..37].join("\t")).is_err());
    for value in ["true", "{}", "{\"root\":\"foreign\"}"] {
        let mut changed = fields.clone();
        changed[37] = value;
        assert!(decode_agent_session_metadata(&changed.join("\t")).is_err());
    }
    record.budget_reference.as_mut().unwrap().conversation = "another".into();
    assert!(encode_agent_session_metadata(&record).is_err());
}
