//! Session-admitted public trust cards. Local verification never enters exports.

use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::{
    identity::account::AccountPublic,
    node::{Introduction, TimestampWitness, TrustCard},
};
use serde::{Deserialize, Serialize};

// JSON arrays expand avatar bytes; bound the entire input before allocating a tree.
const MAX_ARTIFACT_JSON: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct ArtifactEnvelope {
    schema: u32,
    #[serde(flatten)]
    artifact: TrustArtifact,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
enum TrustArtifact {
    Card(TrustCard),
    Introduction(Introduction),
    Witness(TimestampWitness),
}

fn invalid(message: &str) -> CommandError {
    CommandError::Validation(message.into())
}

fn parse_artifact(json: &str) -> Result<TrustArtifact, CommandError> {
    if json.len() > MAX_ARTIFACT_JSON {
        return Err(invalid("signed artifact exceeds 1 MiB"));
    }
    let envelope: ArtifactEnvelope =
        serde_json::from_str(json).map_err(|_| invalid("invalid signed artifact JSON"))?;
    if envelope.schema != 1 {
        return Err(invalid("unsupported signed artifact version"));
    }
    let valid = match &envelope.artifact {
        TrustArtifact::Card(card) => card.verify(),
        TrustArtifact::Introduction(proof) => proof.verify(),
        TrustArtifact::Witness(proof) => proof.verify(),
    };
    if !valid {
        return Err(invalid("invalid signature or signed artifact bounds"));
    }
    Ok(envelope.artifact)
}

fn export_artifact(artifact: TrustArtifact) -> Result<String, CommandError> {
    serde_json::to_string(&ArtifactEnvelope {
        schema: 1,
        artifact,
    })
    .map_err(|_| CommandError::Internal("could not encode signed artifact".into()))
}

fn issuer_id(key: &[u8; 32]) -> String {
    AccountPublic { ed25519_pub: *key }.account_id()
}

#[derive(Serialize)]
pub struct IntroductionView {
    issuer_account_id: String,
    issuer_verified: bool,
    issued_at_ms: u64,
    note: String,
}

#[derive(Serialize)]
pub struct WitnessView {
    issuer_account_id: String,
    issuer_verified: bool,
    observed_at_ms: u64,
}

#[derive(Serialize)]
pub struct TrustCardView {
    account_id: String,
    revision: u64,
    display_name: String,
    bio: String,
    created_at_ms: Option<u64>,
    avatar: Option<String>,
    introductions: Vec<IntroductionView>,
    witnesses: Vec<WitnessView>,
    signed_json: String,
}

fn card_view(card: TrustCard) -> Result<TrustCardView, CommandError> {
    if !card.verify() {
        return Err(invalid("invalid stored trust card"));
    }
    let signed_json = export_artifact(TrustArtifact::Card(card.clone()))?;
    Ok(TrustCardView {
        account_id: card.account_id(),
        revision: card.revision,
        display_name: card.display_name,
        bio: card.bio,
        created_at_ms: card.created_at_ms,
        avatar: card.avatar.and_then(|bytes| String::from_utf8(bytes).ok()),
        introductions: card
            .introductions
            .into_iter()
            .map(|p| IntroductionView {
                issuer_account_id: issuer_id(&p.issuer_ed25519_pub),
                issuer_verified: false,
                issued_at_ms: p.issued_at_ms,
                note: p.note,
            })
            .collect(),
        witnesses: card
            .witnesses
            .into_iter()
            .map(|p| WitnessView {
                issuer_account_id: issuer_id(&p.issuer_ed25519_pub),
                issuer_verified: false,
                observed_at_ms: p.observed_at_ms,
            })
            .collect(),
        signed_json,
    })
}

fn validate_account(account_id: &str) -> Result<(), CommandError> {
    if account_id.len() != 32 || hex::decode(account_id).is_err() {
        return Err(invalid("invalid mesh account ID"));
    }
    Ok(())
}

#[tauri::command]
pub async fn get_trust_card(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    account_id: String,
) -> Result<Option<TrustCardView>, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    validate_account(&account_id)?;
    admission
        .blocking(move |runtime, _| {
            let mut view = runtime
                .handle()
                .trust_card(&account_id)
                .map(card_view)
                .transpose()?;
            if let Some(view) = &mut view {
                let path =
                    crate::config_store::account_path(&crate::data_dir(), &owner, "trust.json")
                        .ok_or_else(|| invalid("invalid account owner"))?;
                let verified = crate::trust::verified_account_ids_at(&path);
                for proof in &mut view.introductions {
                    proof.issuer_verified = verified.contains(&proof.issuer_account_id);
                }
                for proof in &mut view.witnesses {
                    proof.issuer_verified = verified.contains(&proof.issuer_account_id);
                }
            }
            Ok(view)
        })
        .await
}

#[tauri::command]
pub async fn publish_trust_card(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    display_name: String,
    bio: String,
    created_at_ms: Option<u64>,
) -> Result<(), CommandError> {
    RuntimeAdmission::capture(&app, &state, &owner)?
        .detached(move |runtime, admission| {
            Box::pin(async move {
                runtime
                    .handle()
                    .publish_trust_card_if(display_name, bio, created_at_ms, |commit| {
                        admission.authorize_accept(runtime, commit)
                    })
                    .await
                    .map_err(CommandError::from)
            })
        })
        .await
}

/// Import signatures as evidence only. Manual verification is a separate command.
#[tauri::command]
pub async fn import_trust_artifact(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    json: String,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    let artifact = parse_artifact(&json)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                let node = runtime.handle();
                match artifact {
                    TrustArtifact::Card(card) => {
                        node.import_trust_card_if(card, |commit| {
                            admission.authorize_accept(runtime, commit)
                        })
                        .await
                    }
                    TrustArtifact::Introduction(proof) => {
                        node.attach_introduction_if(proof, |commit| {
                            admission.authorize_accept(runtime, commit)
                        })
                        .await
                    }
                    TrustArtifact::Witness(proof) => {
                        node.attach_timestamp_witness_if(proof, |commit| {
                            admission.authorize_accept(runtime, commit)
                        })
                        .await
                    }
                }
                .map_err(CommandError::from)
            })
        })
        .await
}

#[tauri::command]
pub async fn create_trust_introduction(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    account_id: String,
    note: String,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    validate_account(&account_id)?;
    admission
        .blocking(move |runtime, _| {
            let proof = runtime
                .handle()
                .create_introduction(&account_id, note)
                .map_err(CommandError::from)?;
            export_artifact(TrustArtifact::Introduction(proof))
        })
        .await
}

#[tauri::command]
pub async fn witness_trust_creation(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, NodeState>,
    owner: String,
    account_id: String,
) -> Result<String, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &state, &owner)?;
    validate_account(&account_id)?;
    admission
        .blocking(move |runtime, _| {
            let proof = runtime
                .handle()
                .witness_creation(&account_id)
                .map_err(CommandError::from)?;
            export_artifact(TrustArtifact::Witness(proof))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::identity::account::Account;

    fn card() -> TrustCard {
        TrustCard::new(
            &Account::generate(),
            1,
            "Alice".into(),
            "Hello".into(),
            None,
            None,
        )
    }

    #[test]
    fn signed_card_export_round_trips_without_private_verification() {
        let card = card();
        let json = export_artifact(TrustArtifact::Card(card.clone())).unwrap();
        let TrustArtifact::Card(decoded) = parse_artifact(&json).unwrap() else {
            panic!("wrong artifact")
        };
        assert_eq!(decoded, card);
        assert!(!json.contains("verified"));
        assert!(!json.contains("first_seen"));
        assert!(!json.contains("device_changes"));
        let view = card_view(card).unwrap();
        assert_eq!(view.created_at_ms, None);
        assert!(view.introductions.is_empty());
        assert!(view.witnesses.is_empty());
    }

    #[test]
    fn imports_reject_forgery_unknown_versions_and_excess_input() {
        let mut card = card();
        card.bio = "Forged".into();
        let json = export_artifact(TrustArtifact::Card(card)).unwrap();
        assert!(parse_artifact(&json).is_err());
        let valid = export_artifact(TrustArtifact::Card(self::card())).unwrap();
        assert!(parse_artifact(&valid.replace("\"schema\":1", "\"schema\":2")).is_err());
        assert!(parse_artifact(&" ".repeat(MAX_ARTIFACT_JSON + 1)).is_err());
        assert!(parse_artifact("{}").is_err());
    }

    #[test]
    fn introduction_and_time_evidence_keep_their_issuer_and_subject() {
        let issuer = Account::generate();
        let subject = Account::generate().account_id();
        let intro = Introduction::new(&issuer, subject.clone(), "Met in person".into(), 100);
        let json = export_artifact(TrustArtifact::Introduction(intro.clone())).unwrap();
        let TrustArtifact::Introduction(decoded) = parse_artifact(&json).unwrap() else {
            panic!("wrong artifact")
        };
        assert_eq!(decoded, intro);
        let witness = TimestampWitness::new(&issuer, subject, 42, 101);
        let json = export_artifact(TrustArtifact::Witness(witness.clone())).unwrap();
        let TrustArtifact::Witness(decoded) = parse_artifact(&json).unwrap() else {
            panic!("wrong artifact")
        };
        assert_eq!(decoded, witness);
        assert_eq!(issuer_id(&decoded.issuer_ed25519_pub), issuer.account_id());
    }
}
