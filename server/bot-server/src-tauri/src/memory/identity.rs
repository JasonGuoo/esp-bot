//! Explicitly enrolled, local-only templates. Detection/ASR is provided by the
//! local model worker; this module only stores and compares its embeddings.

use rusqlite::{params, OptionalExtension, Row};
use uuid::Uuid;

use super::{
    store::{bump_revision, clean_text, db_error, enum_column, now_ms, profile_exists, validate_id},
    types::*,
    Store,
};

const MIN_DIMENSION: usize = 32;
const MAX_DIMENSION: usize = 4096;

impl Store {
    /// A fresh complete batch replaces one modality only when explicitly asked.
    /// Samples must have passed worker quality gates and refer to the SAME
    /// model weights and preprocessing. Neither face nor voice proves consent.
    pub fn enroll(&self, request: EnrollmentRequest) -> Result<Enrollment, String> {
        if !request.guardian_confirmed {
            return Err("guardian_confirmation_required: enrollment must be approved in the local parent console".into());
        }
        validate_id(&request.profile_id)?;
        let model_id = checked_model_id(&request.model_id)?;
        if request.samples.len() < self.config.min_enrollment_samples
            || request.samples.len() > self.config.max_enrollment_samples
        {
            return Err(format!("enrollment requires {}..{} distinct quality-approved samples",
                self.config.min_enrollment_samples, self.config.max_enrollment_samples));
        }
        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(request.samples.len());
        for sample in request.samples {
            if !sample.quality_ok {
                return Err("enrollment_sample_rejected: local model quality gates did not pass".into());
            }
            let vector = normalize(&sample.embedding)?;
            if let Some(first) = vectors.first() {
                if vector.len() != first.len() {
                    return Err("all enrollment samples must have the same dimension".into());
                }
            }
            if vectors.iter().any(|previous| previous.iter().zip(&vector)
                .all(|(a, b)| (*a - *b).abs() <= 1e-6))
            {
                return Err("duplicate_enrollment_sample: provide different captures, not repeated copies".into());
            }
            vectors.push(vector);
        }
        let dimension = vectors.first().ok_or("enrollment has no samples")?.len();
        let centroid = centroid(&vectors, dimension)?;
        let consistency = match request.kind {
            IdentityKind::Face => self.config.face_enrollment_consistency,
            IdentityKind::Speaker => self.config.speaker_enrollment_consistency,
        };
        if vectors.iter().any(|sample| cosine(sample, &centroid) < consistency) {
            return Err("inconsistent_enrollment: samples may contain different people or unsuitable captures".into());
        }

        let mut db = self.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        profile_exists(&tx, &request.profile_id)?;
        let existing: Option<String> = tx.query_row(
            "SELECT id FROM enrollments WHERE profile_id = ?1 AND kind = ?2",
            params![request.profile_id, request.kind.as_str()], |r| r.get(0),
        ).optional().map_err(db_error)?;
        if existing.is_some() && !request.replace_existing {
            return Err("enrollment_exists: replacing it requires explicit parent-console confirmation".into());
        }
        let previous = {
            let mut statement = tx.prepare(
                "SELECT profile_id, dimension, centroid FROM enrollments WHERE kind = ?1 AND model_id = ?2",
            ).map_err(db_error)?;
            let rows = statement.query_map(params![request.kind.as_str(), model_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?))
            }).map_err(db_error)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?
        };
        for (profile, stored_dimension, bytes) in previous {
            if stored_dimension != dimension as i64 {
                return Err("model_dimension_changed: use a new model_id and re-enroll for changed preprocessing/weights".into());
            }
            if profile != request.profile_id && cosine(&centroid, &decode(&bytes, dimension)?) >= 0.99999 {
                return Err("duplicate_identity_template: the near-identical template is already labeled as another profile".into());
            }
        }
        if let Some(id) = existing {
            tx.execute("DELETE FROM enrollments WHERE id = ?1", [id]).map_err(db_error)?;
        }
        let now = now_ms();
        let enrollment = Enrollment {
            id: format!("enroll_{}", Uuid::new_v4().simple()),
            profile_id: request.profile_id,
            kind: request.kind,
            model_id,
            dimension,
            sample_count: vectors.len(),
            consent_at_ms: now,
            created_at_ms: now,
        };
        tx.execute(
            "INSERT INTO enrollments(id, profile_id, kind, model_id, dimension, sample_count, centroid, consent_at_ms, created_at_ms)\n\
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![enrollment.id, enrollment.profile_id, enrollment.kind.as_str(), enrollment.model_id,
                enrollment.dimension as i64, enrollment.sample_count as i64,
                encode(&centroid), enrollment.consent_at_ms, enrollment.created_at_ms],
        ).map_err(db_error)?;
        for (index, vector) in vectors.iter().enumerate() {
            tx.execute(
                "INSERT INTO embedding_samples(enrollment_id, ordinal, embedding) VALUES (?1, ?2, ?3)",
                params![enrollment.id, index as i64, encode(vector)],
            ).map_err(db_error)?;
        }
        bump_revision(&tx)?;
        tx.commit().map_err(db_error)?;
        Ok(enrollment)
    }

    /// Metadata only: templates are deliberately not exposed to console/tools.
    pub fn list_enrollments(&self, profile_id: Option<&str>) -> Result<Vec<Enrollment>, String> {
        if let Some(profile) = profile_id { validate_id(profile)?; }
        let db = self.connection()?;
        let mut statement = db.prepare(
            "SELECT id, profile_id, kind, model_id, dimension, sample_count, consent_at_ms, created_at_ms\n\
             FROM enrollments WHERE (?1 IS NULL OR profile_id = ?1) ORDER BY profile_id, kind",
        ).map_err(db_error)?;
        let rows = statement.query_map(params![profile_id], enrollment_row).map_err(db_error)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)
    }

    pub fn revoke_enrollment(&self, profile_id: &str, kind: IdentityKind) -> Result<(), String> {
        validate_id(profile_id)?;
        let mut db = self.connection()?;
        let tx = db.transaction().map_err(db_error)?;
        // The foreign key cascade removes individual samples in the same commit.
        tx.execute("DELETE FROM enrollments WHERE profile_id = ?1 AND kind = ?2",
            params![profile_id, kind.as_str()]).map_err(db_error)?;
        bump_revision(&tx)?;
        tx.commit().map_err(db_error)
    }

    /// Recognition is stateless. An ambiguous/missing/low-quality observation
    /// cannot retain the previous person's identity, wake the bot, or unlock
    /// private memory. Scores are cosine similarities, never percentages.
    pub fn identify(&self, request: IdentityRequest) -> Result<MatchResult, String> {
        let model_id = checked_model_id(&request.model_id)?;
        let threshold = match request.kind {
            IdentityKind::Face => self.config.face_threshold,
            IdentityKind::Speaker => self.config.speaker_threshold,
        };
        let result = |status, profile_id, best_score, runner_up_score, reason: &str| MatchResult {
            status, profile_id, best_score, runner_up_score, threshold,
            required_margin: self.config.match_margin, reason: reason.into(),
            authorizes_profile_memory: false,
        };
        if !request.quality_ok {
            return Ok(result(MatchStatus::LowQuality, None, None, None, "local_quality_gate_failed"));
        }
        let query = normalize(&request.embedding)?;
        let entries = {
            let db = self.connection()?;
            let mut statement = db.prepare(
                "SELECT profile_id, dimension, centroid FROM enrollments\n\
                 WHERE kind = ?1 AND model_id = ?2 AND sample_count >= ?3",
            ).map_err(db_error)?;
            let rows = statement.query_map(params![request.kind.as_str(), model_id,
                self.config.min_enrollment_samples as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?))
            }).map_err(db_error)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(db_error)?
        };
        if entries.is_empty() {
            return Ok(result(MatchStatus::NoEnrollment, None, None, None, "no_enrollment_for_exact_model_id"));
        }
        let mut scores = Vec::with_capacity(entries.len());
        for (profile, dimension, bytes) in entries {
            if dimension != query.len() as i64 {
                return Err("model_dimension_mismatch: exact model and preprocessing must match enrollment".into());
            }
            let template = decode(&bytes, query.len())?;
            scores.push((profile, cosine(&query, &template)));
        }
        scores.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let Some((profile, best)) = scores.first() else {
            return Ok(result(MatchStatus::NoEnrollment, None, None, None, "no_usable_enrollment"));
        };
        let runner = scores.get(1).map(|candidate| candidate.1);
        if *best < threshold {
            return Ok(result(MatchStatus::Unknown, None, Some(*best), runner, "below_cosine_threshold"));
        }
        if runner.is_some_and(|score| *best - score < self.config.match_margin) {
            return Ok(result(MatchStatus::Ambiguous, None, Some(*best), runner, "nearest_profile_margin_too_small"));
        }
        Ok(result(MatchStatus::Matched, Some(profile.clone()), Some(*best), runner,
            "candidate_only_requires_explicit_profile_selection"))
    }
}

/// Optional conservative fusion for a console suggestion. Conflicting or
/// contradictory evidence becomes unknown; never average identities together.
pub fn fuse_identity(face: Option<&MatchResult>, speaker: Option<&MatchResult>) -> FusedIdentity {
    let candidate = |observation: Option<&MatchResult>| {
        observation.filter(|value| value.status == MatchStatus::Matched)
            .and_then(|value| value.profile_id.clone())
    };
    let uncertain_evidence = |observation: Option<&MatchResult>| {
        observation.is_some_and(|value| matches!(value.status, MatchStatus::Unknown | MatchStatus::Ambiguous))
    };
    let (status, profile_id, reason) = match (candidate(face), candidate(speaker)) {
        (Some(a), Some(b)) if a == b => ("agreement", Some(a), "both_modalities_suggest_the_same_profile"),
        (Some(_), Some(_)) => ("conflict", None, "face_and_speaker_disagree"),
        (Some(a), None) if !uncertain_evidence(speaker) => ("single_candidate", Some(a), "face_only_candidate"),
        (None, Some(b)) if !uncertain_evidence(face) => ("single_candidate", Some(b), "speaker_only_candidate"),
        _ => ("unknown", None, "insufficient_or_contradictory_evidence"),
    };
    FusedIdentity { status: status.into(), profile_id, reason: reason.into(), authorizes_profile_memory: false }
}

fn checked_model_id(value: &str) -> Result<String, String> {
    let model_id = clean_text(value, 256, false, "model_id")?;
    if !model_id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_/.:@+".contains(&byte)) {
        return Err("model_id must be a stable ASCII weights/preprocessing identifier".into());
    }
    Ok(model_id)
}

fn normalize(input: &[f32]) -> Result<Vec<f32>, String> {
    if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&input.len()) || input.iter().any(|value| !value.is_finite()) {
        return Err("embedding must contain 32..4096 finite float values".into());
    }
    let norm = input.iter().map(|value| (*value as f64).powi(2)).sum::<f64>().sqrt();
    if !norm.is_finite() || norm <= 1e-12 {
        return Err("embedding has zero or invalid magnitude".into());
    }
    Ok(input.iter().map(|value| (*value as f64 / norm) as f32).collect())
}

fn centroid(vectors: &[Vec<f32>], dimension: usize) -> Result<Vec<f32>, String> {
    let mut average = vec![0.0f64; dimension];
    for vector in vectors {
        for (sum, value) in average.iter_mut().zip(vector) { *sum += *value as f64; }
    }
    let average: Vec<f32> = average.into_iter().map(|sum| (sum / vectors.len() as f64) as f32).collect();
    normalize(&average)
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum::<f64>().clamp(-1.0, 1.0) as f32
}

fn encode(vector: &[f32]) -> Vec<u8> {
    vector.iter().flat_map(|value| value.to_le_bytes()).collect()
}

fn decode(bytes: &[u8], dimension: usize) -> Result<Vec<f32>, String> {
    if !(MIN_DIMENSION..=MAX_DIMENSION).contains(&dimension) || bytes.len() != dimension * 4 {
        return Err("stored identity template has an invalid dimension".into());
    }
    let vector: Vec<_> = bytes.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).collect();
    normalize(&vector)
}

fn enrollment_row(row: &Row<'_>) -> rusqlite::Result<Enrollment> {
    Ok(Enrollment {
        id: row.get(0)?, profile_id: row.get(1)?, kind: enum_column(row, 2)?, model_id: row.get(3)?,
        dimension: row.get::<_, i64>(4)? as usize,
        sample_count: row.get::<_, i64>(5)? as usize,
        consent_at_ms: row.get(6)?, created_at_ms: row.get(7)?,
    })
}
