//! A run owns its frozen plan, automatic starts, checkpoints and immutable trials.
use super::{analysis, io::Provider, probe::Probe, *};
use crate::{
    correction::{
        Correction, Curve,
        room::{self, Frozen, Realization, Recipe},
    },
    domain::Rate,
    error::{Result, invalid},
    storage::{ContentHash, Store},
};
use serde::{Deserialize, Serialize};
use std::{
    sync::mpsc::{Receiver, RecvTimeoutError},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
pub enum Fault {
    #[error("Verification time budget exhausted; previous result retained")]
    TimeBudget,
    #[error("Measurement transport fault: {0}")]
    Transport(String),
    #[error("Measurement timing invalid: {0}")]
    Timing(String),
    #[error("Measurement quality invalid: {0}")]
    Quality(String),
    #[error("Measurement stopped; output remains muted")]
    Stopped,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum Progression {
    #[default]
    Manual,
    HandsFree {
        movement_seconds: f64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunKind {
    Measure,
    ResumeMeasurement,
    Verify { pass: PassId },
    VerifySpatial { pass: PassId },
}
impl RunKind {
    pub fn acquires(self) -> bool {
        matches!(self, Self::Measure | Self::ResumeMeasurement)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PointCheckpoint {
    pub noise: Option<Noise>,
    pub full: [Option<Take>; 2],
    pub lf: [Option<Take>; 2],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionCheckpoint {
    pub plan: SourcePlan,
    pub points: Vec<PointCheckpoint>,
    pub repeat_reference: [Option<Take>; 2],
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "schema", content = "checkpoint")]
pub enum Recovery {
    #[serde(rename = "spatial-1")]
    Spatial {
        session: ContentHash,
        candidate: ContentHash,
        objective: ContentHash,
        completed: Vec<[Take; 2]>,
    },
    #[serde(rename = "acquisition-1")]
    Acquisition(Box<AcquisitionCheckpoint>),
    #[serde(rename = "session-1")]
    Session(ContentHash),
}
impl Recovery {
    pub fn source_plan(&self, store: &Store) -> Result<SourcePlan> {
        let plan = match self {
            Self::Acquisition(checkpoint) => checkpoint.plan.clone(),
            Self::Session(id) | Self::Spatial { session: id, .. } => {
                Source::load(Session::load(*id, store)?.source, store)?.plan
            }
        };
        plan.validate()?;
        Ok(plan)
    }

    pub fn load(store: &Store) -> Result<Option<Self>> {
        match std::fs::read(store.root().join("room-recovery.json")) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    fn save(&self, store: &Store) -> Result<()> {
        store.write("room-recovery.json", &serde_json::to_vec(self)?)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub levels: Option<super::levels::Checks>,
    pub source: SourcePlan,
    pub recipe: Recipe,
    pub progression: Progression,
    pub kind: RunKind,
    pub session: Option<ContentHash>,
}
impl Plan {
    pub fn validate(&self) -> Result<()> {
        self.source.validate()?;
        if let Some(levels) = &self.levels {
            levels.validate(&self.source)?;
        }
        self.recipe.validate()?;
        if let Progression::HandsFree { movement_seconds } = self.progression
            && !(5.0..=120.0).contains(&movement_seconds)
        {
            return Err(invalid("Movement interval must be 5-120 seconds"));
        }
        if self.kind.acquires() && self.session.is_some() {
            return Err(invalid("New acquisition uses a new source session"));
        }
        if !self.kind.acquires() && self.session.is_none() {
            return Err(invalid("Verification needs a saved source session"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Movement {
    Acquisition(usize),
    Reference(usize),
    SpatialVerification(usize),
}
impl Movement {
    pub fn position(self) -> usize {
        match self {
            Self::Acquisition(p) | Self::Reference(p) | Self::SpatialVerification(p) => p,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveAction {
    Ready,
    Pause,
    Resume,
}
#[derive(Clone, Copy, Debug)]
pub struct Control {
    pub movement: Movement,
    pub action: MoveAction,
}
#[derive(Clone, Copy, Debug)]
pub enum ProbePurpose {
    Baseline,
    Verification,
}
impl std::fmt::Display for ProbePurpose {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Baseline => f.write_str("Baseline"),
            Self::Verification => f.write_str("Verification"),
        }
    }
}
#[derive(Clone, Debug)]
pub enum Progress {
    Levels {
        speaker: Speaker,
    },
    Source {
        session: ContentHash,
    },
    Noise {
        position: usize,
        noise: Box<Noise>,
    },
    Background {
        position: usize,
    },
    Sweep {
        position: usize,
        speaker: Speaker,
        purpose: ProbePurpose,
        band: Band,
    },
    Move {
        movement: Movement,
        seconds: Option<f64>,
        paused: bool,
    },
    Fitting {
        band: Band,
    },
    Pass {
        session: ContentHash,
        record: Box<Pass>,
    },
    Finished {
        session: ContentHash,
        reason: StopReason,
    },
    Failed(String),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    Prepared,
    ProcessedCorrectionVerified,
    ImprovedLimitsReached,
    NoMeasurableImprovement,
    TimeLimit,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassId(pub usize);
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Scope {
    SpatialPartial {
        positions: Vec<usize>,
        required: Vec<usize>,
        trusted: Band,
    },
    Prediction {
        band: Band,
    },
    ReferenceFull {
        position: usize,
        trusted: Band,
    },
    SpatialFull {
        positions: Vec<usize>,
        trusted: Band,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Baseline,
    Prepared,
    Accepted,
    Rejected,
    Provisional,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metrics {
    pub processed_rms_db: [f64; 2],
    pub requested_rms_db: [f64; 2],
    pub target_rms_db: [f64; 2],
    pub percentile90_db: [f64; 2],
    pub coverage: [f64; 2],
    pub repeatability_db: f64,
    pub noise_uncertainty_db: f64,
    pub unexpected_outside_fit_db: f64,
    pub alignment_error_ms: f64,
    pub per_position_rms: Vec<[f64; 2]>,
}
impl Metrics {
    pub fn decision_uncertainty(&self) -> f64 {
        self.repeatability_db.max(self.noise_uncertainty_db)
    }
    pub fn score(&self) -> f64 {
        ((self.processed_rms_db[0].powi(2) + self.processed_rms_db[1].powi(2)) * 0.5).sqrt()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Artifact {
    Baseline,
    Candidate {
        correction: ContentHash,
        design_db: [Vec<f32>; 2],
        realized: Realization,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Context {
    Offline,
    Measured { run: RunId, settings: ContentHash },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RunId(std::num::NonZeroU64);
impl RunId {
    pub fn number(self) -> u64 {
        self.0.get()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum RunRecipe {
    #[serde(rename = "1")]
    V1 { progression: Progression },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trial {
    pub parent: Option<PassId>,
    pub objective: ContentHash,
    pub artifact: Artifact,
    pub changed: Vec<Band>,
    pub decision: Decision,
    pub scope: Scope,
    pub metrics: Metrics,
    pub measured: Vec<[Take; 2]>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pass {
    pub id: PassId,
    pub context: Context,
    #[serde(flatten)]
    pub trial: Trial,
}
impl std::ops::Deref for Pass {
    type Target = Trial;
    fn deref(&self) -> &Trial {
        &self.trial
    }
}
impl Pass {
    pub fn correction(&self) -> Option<ContentHash> {
        match self.artifact {
            Artifact::Baseline => None,
            Artifact::Candidate { correction, .. } => Some(correction),
        }
    }
    pub fn fully_verified(&self) -> bool {
        !self.measured.is_empty()
            && !matches!(
                self.scope,
                Scope::Prediction { .. } | Scope::SpatialPartial { .. }
            )
            && !matches!(self.decision, Decision::Prepared | Decision::Provisional)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub schema: u32,
    pub source: ContentHash,
    pub passes: Vec<ContentHash>,
    pub best: Option<PassId>,
    pub preferred: Option<PassId>,
    pub outcomes: std::collections::BTreeMap<RunId, StopReason>,
}
impl Session {
    /// A deliberate unverified draft can be resumed; rejected trials stay in history.
    pub fn default_candidate(&self, history: &[Pass]) -> Option<PassId> {
        assert_eq!(
            history.len(),
            self.passes.len(),
            "history belongs to session"
        );
        history
            .iter()
            .rev()
            .find(|pass| {
                pass.correction().is_some()
                    && (pass.decision == Decision::Prepared
                        || pass.decision == Decision::Accepted && pass.fully_verified())
            })
            .map(|pass| pass.id)
    }

    /// Preserve preference changes made while an append-only preparation was running.
    pub fn rebase_preference(&mut self, current: &Self) -> Result<()> {
        if self.source != current.source || !self.passes.starts_with(&current.passes) {
            return Err(invalid(
                "Prepared history diverged from the current room session",
            ));
        }
        if current
            .preferred
            .is_some_and(|id| id.0 >= current.passes.len())
        {
            return Err(invalid("Invalid listening preference"));
        }
        self.preferred = current.preferred;
        Ok(())
    }

    pub fn load(id: ContentHash, store: &Store) -> Result<Self> {
        let session: Self = serde_json::from_slice(&store.get(&id)?)?;
        if session.schema != 1
            || session.best.is_some_and(|p| p.0 >= session.passes.len())
            || session
                .preferred
                .is_some_and(|p| p.0 >= session.passes.len())
        {
            return Err(invalid("Invalid room session manifest"));
        }
        Source::load(session.source, store)?;
        Ok(session)
    }
    pub fn save(&self, store: &Store) -> Result<ContentHash> {
        store.put(&serde_json::to_vec(self)?)
    }
    pub fn history(&self, store: &Store) -> Result<Vec<Pass>> {
        self.passes
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let pass: Pass = serde_json::from_slice(&store.get(id)?)?;
                if pass.id.0 != i || pass.parent.is_some_and(|p| p.0 >= i) {
                    return Err(invalid("Invalid room pass ancestry"));
                }
                let frozen:Frozen=serde_json::from_slice(&store.get(&pass.objective)?)?;frozen.validate()?;
                if frozen.source!=self.source{return Err(invalid("Pass objective references another measurement source"));}
                if let Context::Measured{settings,..}=pass.context{let _: RunRecipe = serde_json::from_slice(&store.get(&settings)?)?;}
                if let Artifact::Candidate{correction,design_db,realized}=&pass.artifact {
                    let filter:Correction=serde_json::from_slice(&store.get(correction)?)?;filter.validate()?;realized.validate()?;
                    if let crate::correction::Origin::Room{objective,design_db:stored,..}=&filter.origin{if *objective!=pass.objective||stored!=design_db||filter.rate!=frozen.rate{return Err(invalid("History filter differs from its frozen recipe"));}}else{return Err(invalid("Room history contains another correction source"));}
                }
                let candidate=pass.correction();
                for pair in &pass.measured{for(c,take)in pair.iter().enumerate(){take.validate()?;if take.speaker.index()!=c||take.position>=frozen.baseline.points.len()||take.candidate!=candidate||take.probe.rate!=frozen.rate{return Err(invalid("Verification does not identify this exact filter, rate or position"));}}}
                if matches!(pass.scope,Scope::Prediction{..})&&!pass.measured.is_empty(){return Err(invalid("Prediction cannot inherit measurement evidence"));}
                let m=&pass.metrics;
                if m.processed_rms_db.iter().chain(&m.requested_rms_db).chain(&m.target_rms_db).chain(&m.percentile90_db).any(|v|!v.is_finite()||*v<0.0)||m.coverage.iter().any(|v|!v.is_finite()||!(0.0..=1.0).contains(v))||!m.repeatability_db.is_finite()||!m.noise_uncertainty_db.is_finite()||!m.alignment_error_ms.is_finite(){return Err(invalid("Invalid history confidence or error metrics"));}
                Ok(pass)
            })
            .collect()
    }
    pub fn prefer(&mut self, id: PassId, store: &Store) -> Result<ContentHash> {
        let history = self.history(store)?;
        let p = history
            .get(id.0)
            .ok_or_else(|| invalid("Unknown history pass"))?;
        if let Some(id) = p.correction() {
            let correction: Correction = serde_json::from_slice(&store.get(&id)?)?;
            correction.validate()?;
        }
        self.preferred = Some(id);
        self.save(store)
    }
}

pub struct Run<'a> {
    pub plan: Plan,
    pub store: &'a Store,
    pub cancel: crate::audio::Cancellation,
    pub controls: Receiver<Control>,
    pub progress: Box<dyn Fn(Progress) + Send + 'a>,
    pub provider: Box<dyn Provider>,
}
impl Run<'_> {
    fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Fault::Stopped.into());
        }
        Ok(())
    }
    fn emit(&self, p: Progress) {
        (self.progress)(p);
    }
    fn move_to(&self, movement: Movement) -> Result<()> {
        let mut remaining = match self.plan.progression {
            Progression::Manual => None,
            Progression::HandsFree { movement_seconds } => Some(movement_seconds),
        };
        let mut paused = false;
        let mut last = Instant::now();
        loop {
            self.check()?;
            self.emit(Progress::Move {
                movement,
                seconds: remaining,
                paused,
            });
            match self.controls.recv_timeout(Duration::from_millis(100)) {
                Ok(control) if control.movement != movement => {}
                Ok(Control {
                    action: MoveAction::Ready,
                    ..
                }) => return self.check(),
                Ok(Control {
                    action: MoveAction::Pause,
                    ..
                }) => paused = true,
                Ok(Control {
                    action: MoveAction::Resume,
                    ..
                }) => paused = false,
                Err(RecvTimeoutError::Disconnected) => return Err(Fault::Stopped.into()),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            if !paused && let Some(time) = &mut remaining {
                *time -= now.duration_since(last).as_secs_f64();
                if *time <= 0.0 {
                    return self.check();
                }
            }
            last = now;
        }
    }
    fn background(&mut self, position: usize) -> Result<Noise> {
        self.emit(Progress::Background { position });
        self.check()?;
        let frames = (self.plan.source.acquisition.noise_seconds
            * f64::from(self.plan.source.rate.hz())) as usize;
        let silence = vec![[0.0; 2]; frames];
        let recording = self.provider.record(&silence, position, &self.cancel)?;
        recording.validate(frames)?;
        let noise = analysis::noise(
            &recording.samples,
            recording.rate,
            self.plan.source.acquisition.noise_limits,
            self.store,
        )?;
        self.emit(Progress::Noise {
            position,
            noise: Box::new(noise.clone()),
        });
        Ok(noise)
    }
    fn take(
        &mut self,
        position: usize,
        speaker: Speaker,
        noise: &Noise,
        candidate: Option<(ContentHash, &Correction)>,
        settings: &Acquisition,
        purpose: ProbePurpose,
    ) -> Result<Take> {
        let probe = Probe::new(
            self.plan.source.rate,
            settings,
            &self.plan.source.gain,
            speaker,
            candidate.map(|(_, c)| c),
        )?;
        let output_bytes: Vec<_> = probe
            .output
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(
            self.store.put(&output_bytes)?,
            probe.output_identity,
            "probe output identity"
        );
        for retry in 0..=1 {
            self.check()?;
            self.emit(Progress::Sweep {
                position,
                speaker,
                purpose,
                band: settings.band,
            });
            let recording = self
                .provider
                .record(&probe.output, position, &self.cancel)?;
            recording.validate(probe.output.len())?;
            match analysis::identify(
                &recording.samples,
                recording.rate,
                &probe,
                noise,
                &self.plan.source.calibration,
                self.store,
            ) {
                Ok((clock, response)) => {
                    return Ok(Take {
                        speaker,
                        position,
                        raw: Waveform::save(&recording.samples, recording.rate, self.store)?,
                        probe: probe.recipe,
                        output_identity: probe.output_identity,
                        candidate: candidate.map(|(id, _)| id),
                        timing: recording.input,
                        output_timing: recording.output,
                        clock,
                        response,
                        noise: noise.clone(),
                    });
                }
                Err(crate::error::Error::Measurement(Fault::Quality(_))) if retry < 1 => {}
                Err(e) => return Err(e),
            }
        }
        unreachable!("bounded take loop returns")
    }
    fn pair(
        &mut self,
        position: usize,
        noise: &Noise,
        candidate: Option<(ContentHash, &Correction)>,
        settings: &Acquisition,
        purpose: ProbePurpose,
    ) -> Result<[Take; 2]> {
        let left = self.take(position, Speaker::Left, noise, candidate, settings, purpose)?;
        let right = self.take(
            position,
            Speaker::Right,
            noise,
            candidate,
            settings,
            purpose,
        )?;
        Ok([left, right])
    }
    fn acquire(&mut self) -> Result<Source> {
        let mut checkpoint = if self.plan.kind == RunKind::ResumeMeasurement {
            let Some(Recovery::Acquisition(mut checkpoint)) = Recovery::load(self.store)? else {
                return Err(invalid(
                    "No interrupted acquisition checkpoint is available",
                ));
            };
            checkpoint.plan.acquisition.noise_limits = self.plan.source.acquisition.noise_limits;
            if ContentHash::of(&serde_json::to_vec(&checkpoint.plan)?)
                != ContentHash::of(&serde_json::to_vec(&self.plan.source)?)
                || checkpoint.points.len() != self.plan.source.layout.positions.len()
            {
                return Err(invalid(
                    "Resume needs the same microphone, route, gain, calibration, positions and acquisition settings",
                ));
            }
            *checkpoint
        } else {
            AcquisitionCheckpoint {
                plan: self.plan.source.clone(),
                points: vec![PointCheckpoint::default(); self.plan.source.layout.positions.len()],
                repeat_reference: [None, None],
            }
        };
        let settings = self.plan.source.acquisition.clone();
        let order = self.plan.source.layout.order.clone();
        let reference = self.plan.source.layout.reference;
        for (ordinal, &position) in order.iter().enumerate() {
            let complete = checkpoint.points[position].full.iter().all(Option::is_some)
                && (settings.lf_detail.is_none()
                    || checkpoint.points[position].lf.iter().all(Option::is_some));
            if complete
                && (position != reference
                    || checkpoint.repeat_reference.iter().all(Option::is_some))
            {
                continue;
            }
            if ordinal > 0 || position != reference || self.plan.kind == RunKind::ResumeMeasurement
            {
                self.move_to(Movement::Acquisition(position))?;
            }
            if checkpoint.points[position].noise.is_none() {
                checkpoint.points[position].noise = Some(self.background(position)?);
                Recovery::Acquisition(Box::new(checkpoint.clone())).save(self.store)?;
            }
            let noise = checkpoint.points[position]
                .noise
                .clone()
                .expect("position noise acquired");
            for speaker in Speaker::ALL {
                let c = speaker.index();
                if checkpoint.points[position].full[c].is_none() {
                    checkpoint.points[position].full[c] = Some(self.take(
                        position,
                        speaker,
                        &noise,
                        None,
                        &settings,
                        ProbePurpose::Baseline,
                    )?);
                    Recovery::Acquisition(Box::new(checkpoint.clone())).save(self.store)?;
                }
            }
            if let Some(lf) = &settings.lf_detail {
                let mut slow = settings.clone();
                slow.band = lf.band;
                slow.sweep_seconds = lf.seconds;
                slow.lf_detail = None;
                for speaker in Speaker::ALL {
                    let c = speaker.index();
                    if checkpoint.points[position].lf[c].is_none() {
                        checkpoint.points[position].lf[c] = Some(self.take(
                            position,
                            speaker,
                            &noise,
                            None,
                            &slow,
                            ProbePurpose::Baseline,
                        )?);
                        Recovery::Acquisition(Box::new(checkpoint.clone())).save(self.store)?;
                    }
                }
            }
            if position == reference {
                for speaker in Speaker::ALL {
                    let c = speaker.index();
                    if checkpoint.repeat_reference[c].is_none() {
                        checkpoint.repeat_reference[c] = Some(self.take(
                            position,
                            speaker,
                            &noise,
                            None,
                            &settings,
                            ProbePurpose::Baseline,
                        )?);
                        Recovery::Acquisition(Box::new(checkpoint.clone())).save(self.store)?;
                    }
                }
            }
        }
        let points = checkpoint
            .points
            .into_iter()
            .enumerate()
            .map(|(position, p)| PositionEvidence {
                position,
                noise: p.noise.expect("complete position noise"),
                full: p.full.map(|t| t.expect("complete channel acquisition")),
                lf: settings
                    .lf_detail
                    .as_ref()
                    .map(|_| p.lf.map(|t| t.expect("complete LF acquisition"))),
            })
            .collect();
        Ok(Source {
            levels: self.plan.levels.clone(),
            schema: 1,
            plan: self.plan.source.clone(),
            points,
            repeat_reference: checkpoint
                .repeat_reference
                .map(|t| t.expect("reference repeat acquired")),
        })
    }
    fn append(&self, session: &mut Session, trial: Trial, best: bool) -> Result<ContentHash> {
        let context = self.store.put(&serde_json::to_vec(&RunRecipe::V1 {
            progression: self.plan.progression.clone(),
        })?)?;
        let history = session.history(self.store)?;
        let previous = history.iter().rev().find_map(|p| match p.context {
            Context::Measured { run, .. } => Some(run),
            Context::Offline => None,
        });
        let run = if trial.decision == Decision::Baseline && trial.parent.is_none() {
            RunId(
                std::num::NonZeroU64::new(previous.map_or(1, |run| run.number() + 1))
                    .expect("positive run number"),
            )
        } else {
            previous.expect("measured trials follow their run baseline")
        };
        let pass = Pass {
            id: PassId(session.passes.len()),
            context: Context::Measured {
                run,
                settings: context,
            },
            trial,
        };
        let id = self.store.put(&serde_json::to_vec(&pass)?)?;
        session.passes.push(id);
        if best {
            assert!(
                pass.fully_verified() || pass.decision == Decision::Baseline,
                "best must have full evidence"
            );
            session.best = Some(pass.id);
        }
        let manifest = session.save(self.store)?;
        Recovery::Session(manifest).save(self.store)?;
        self.emit(Progress::Pass {
            session: manifest,
            record: Box::new(pass),
        });
        Ok(manifest)
    }
    fn verify(
        &mut self,
        source: &Source,
        session: &mut Session,
        selected: PassId,
    ) -> Result<ContentHash> {
        let history = session.history(self.store)?;
        let original = history
            .get(selected.0)
            .ok_or_else(|| invalid("Unknown pass for spatial verification"))?;
        let Artifact::Candidate { correction: id, .. } = &original.artifact else {
            return Err(invalid("Spatial verification needs a correction filter"));
        };
        let id = *id;
        let frozen: Frozen = serde_json::from_slice(&self.store.get(&original.objective)?)?;
        if frozen.source != session.source || frozen.rate != self.plan.source.rate {
            return Err(invalid(
                "Verification source or rate differs from the selected filter",
            ));
        }
        let correction: Correction = serde_json::from_slice(&self.store.get(&id)?)?;
        correction.validate()?;
        let required: Vec<_> = match self.plan.kind {
            RunKind::Verify { .. } => vec![source.plan.layout.reference],
            RunKind::VerifySpatial { .. } => source
                .plan
                .layout
                .order
                .iter()
                .copied()
                .filter(|&p| frozen.baseline.weights[p] > 0.0)
                .collect(),
            _ => unreachable!("verification requires a selected filter"),
        };
        let reference = source.plan.layout.reference;
        let repeat = repeatability(
            &source.points[reference]
                .full
                .each_ref()
                .map(|t| t.response.magnitude.clone()),
            &source
                .repeat_reference
                .each_ref()
                .map(|t| t.response.magnitude.clone()),
            frozen.recipe.range,
        );
        let trusted = source.points[reference].full[0].response.trusted_band;
        let baseline_takes: Vec<_> = required
            .iter()
            .map(|&p| source.points[p].full.clone())
            .collect();
        let baseline_metrics = spatial_metrics(&frozen, source, &baseline_takes, repeat)?;
        let recovery = Recovery::load(self.store)?;
        let mut completed = match recovery {
            Some(Recovery::Spatial {
                session: previous,
                candidate,
                objective,
                completed,
            }) if candidate == id
                && objective == original.objective
                && Session::load(previous, self.store)?.source == session.source =>
            {
                completed
            }
            _ => Vec::new(),
        };
        let mut parent = selected;
        if completed.is_empty() {
            parent = PassId(session.passes.len());
            self.append(
                session,
                Trial {
                    parent: None,
                    objective: original.objective,
                    artifact: Artifact::Baseline,
                    changed: Vec::new(),
                    decision: Decision::Baseline,
                    scope: Scope::SpatialFull {
                        positions: required.clone(),
                        trusted,
                    },
                    metrics: baseline_metrics.clone(),
                    measured: baseline_takes,
                },
                session.best.is_none(),
            )?;
        }
        let start = self.provider.elapsed();
        self.provider
            .set_deadline(start + Duration::from_secs_f64(300.0));
        for &position in &required {
            if completed.iter().any(|p| p[0].position == position) {
                continue;
            }
            self.move_to(Movement::SpatialVerification(position))?;
            let noise = self.background(position)?;
            let take = self.pair(
                position,
                &noise,
                Some((id, &correction)),
                &self.plan.source.acquisition.clone(),
                ProbePurpose::Verification,
            )?;
            completed.push(take);
            let positions = completed.iter().map(|p| p[0].position).collect();
            let metrics = spatial_metrics(&frozen, source, &completed, repeat)?;
            let partial_id = PassId(session.passes.len());
            let manifest = self.append(
                session,
                Trial {
                    parent: Some(parent),
                    objective: original.objective,
                    artifact: original.artifact.clone(),
                    changed: Vec::new(),
                    decision: Decision::Provisional,
                    scope: Scope::SpatialPartial {
                        positions,
                        required: required.clone(),
                        trusted,
                    },
                    metrics,
                    measured: completed.clone(),
                },
                false,
            )?;
            parent = partial_id;
            Recovery::Spatial {
                session: manifest,
                candidate: id,
                objective: original.objective,
                completed: completed.clone(),
            }
            .save(self.store)?;
        }
        let metrics = spatial_metrics(&frozen, source, &completed, repeat)?;
        let threshold = 0.25_f64.max(2.0 * metrics.decision_uncertainty());
        let improved = baseline_metrics.score() - metrics.score() > threshold;
        let stable = metrics
            .per_position_rms
            .iter()
            .zip(&baseline_metrics.per_position_rms)
            .all(|(after, before)| {
                after
                    .iter()
                    .zip(before)
                    .all(|(&a, &b)| a <= b + threshold * 0.5)
            });
        let accurate = metrics
            .processed_rms_db
            .iter()
            .all(|&v| v + 2.0 * metrics.decision_uncertainty() <= 1.5);
        let complete = completed
            .iter()
            .all(|pair| covers_objective(&frozen, source, pair));
        let accepted = complete && stable && (improved || accurate);
        let reason = if accepted {
            if accurate {
                StopReason::ProcessedCorrectionVerified
            } else {
                StopReason::ImprovedLimitsReached
            }
        } else {
            StopReason::NoMeasurableImprovement
        };
        let manifest = self.append(
            session,
            Trial {
                parent: Some(parent),
                objective: original.objective,
                artifact: original.artifact.clone(),
                changed: Vec::new(),
                decision: if !complete {
                    Decision::Provisional
                } else if accepted {
                    Decision::Accepted
                } else {
                    Decision::Rejected
                },
                scope: match self.plan.kind {
                    RunKind::Verify { .. } => Scope::ReferenceFull {
                        position: reference,
                        trusted,
                    },
                    RunKind::VerifySpatial { .. } => Scope::SpatialFull {
                        positions: required,
                        trusted,
                    },
                    _ => unreachable!("verification plan"),
                },
                metrics,
                measured: completed,
            },
            accepted,
        )?;
        let _ = manifest;
        self.finish(session, reason)
    }
    fn finish(&self, session: &mut Session, reason: StopReason) -> Result<ContentHash> {
        if let Some(run) = session
            .history(self.store)?
            .iter()
            .rev()
            .find_map(|p| match p.context {
                Context::Measured { run, .. } => Some(run),
                Context::Offline => None,
            })
        {
            session.outcomes.insert(run, reason);
        }
        let id = session.save(self.store)?;
        Recovery::Session(id).save(self.store)?;
        self.emit(Progress::Finished {
            session: id,
            reason,
        });
        Ok(id)
    }
    pub fn execute(mut self) -> Result<ContentHash> {
        match self.execute_inner() {
            Err(crate::error::Error::Measurement(Fault::TimeBudget)) => {
                let session = match Recovery::load(self.store)? {
                    Some(Recovery::Session(session) | Recovery::Spatial { session, .. }) => session,
                    _ => return Err(Fault::TimeBudget.into()),
                };
                self.emit(Progress::Finished {
                    session,
                    reason: StopReason::TimeLimit,
                });
                Ok(session)
            }
            result => result,
        }
    }
    fn execute_inner(&mut self) -> Result<ContentHash> {
        self.plan.validate()?;
        self.check()?;
        if let Some(id) = self.plan.session {
            let mut session = Session::load(id, self.store)?;
            let source = Source::load(session.source, self.store)?;
            if !source.plan.same_acquisition_path(&self.plan.source)? {
                return Err(invalid(
                    "Restore the source microphone, output, calibration and gain before verification",
                ));
            }
            let acquisition = self.plan.source.acquisition.clone();
            self.plan.source = source.plan.clone();
            self.plan.source.acquisition = acquisition;
            let pass = match self.plan.kind {
                RunKind::Verify { pass } | RunKind::VerifySpatial { pass } => pass,
                _ => unreachable!("validated verification plan"),
            };
            return self.verify(&source, &mut session, pass);
        }
        let source = self.acquire()?;
        let source_id = source.save(self.store)?;
        let mut session = Session {
            schema: 1,
            source: source_id,
            passes: Vec::new(),
            best: None,
            preferred: None,
            outcomes: std::collections::BTreeMap::new(),
        };
        let manifest = session.save(self.store)?;
        Recovery::Session(manifest).save(self.store)?;
        self.emit(Progress::Source { session: manifest });
        self.emit(Progress::Fitting {
            band: self.plan.recipe.range,
        });
        let frozen = room::freeze(&source, source_id, self.plan.recipe.clone())?;
        let objective = self.store.put(&serde_json::to_vec(&frozen)?)?;
        let reference = source.plan.layout.reference;
        let takes = &source.points[reference].full;
        let curves = takes.each_ref().map(|t| t.response.magnitude.clone());
        let repeat = repeatability(
            &curves,
            &source
                .repeat_reference
                .each_ref()
                .map(|t| t.response.magnitude.clone()),
            frozen.recipe.range,
        );
        let mut baseline_metrics = metrics(&frozen, &source, &curves, repeat, None)?;
        apply_evidence(&mut baseline_metrics, &frozen, takes);
        let manifest = self.append(
            &mut session,
            Trial {
                parent: None,
                objective,
                artifact: Artifact::Baseline,
                changed: Vec::new(),
                decision: Decision::Baseline,
                scope: Scope::ReferenceFull {
                    position: reference,
                    trusted: takes[0].response.trusted_band,
                },
                metrics: baseline_metrics,
                measured: vec![takes.clone()],
            },
            true,
        )?;
        let (manifest, pass, _) = prepare(
            manifest,
            self.plan.recipe.clone(),
            Some(PassId(0)),
            self.store,
        )?;
        self.emit(Progress::Pass {
            session: manifest,
            record: Box::new(pass),
        });
        self.check()?;
        self.finish(
            &mut Session::load(manifest, self.store)?,
            StopReason::Prepared,
        )
    }
}

pub fn pair_duration(settings: &Acquisition, rate: Rate) -> f64 {
    pair_duration_with_filter_support(settings, rate, 0)
}
fn pair_duration_with_filter_support(
    settings: &Acquisition,
    rate: Rate,
    filter_tail_frames: usize,
) -> f64 {
    2.0 * super::probe::Recipe::with_filter_support(
        rate,
        settings,
        &GainContract::default(),
        filter_tail_frames,
    )
    .expect("validated acquisition")
    .total_frames as f64
        / f64::from(rate.hz())
}
pub struct Estimate {
    pub acquisition: Duration,
    pub moves: usize,
    pub movement: Option<Duration>,
}
impl Estimate {
    pub fn for_preferences(prefs: &super::Preferences, rate: Rate) -> Result<Self> {
        prefs.layout.validate()?;
        prefs.acquisition.validate(rate)?;
        let count = prefs.layout.positions.len();
        let full = pair_duration(&prefs.acquisition, rate);
        let lf = if let Some(detail) = &prefs.acquisition.lf_detail {
            let mut settings = prefs.acquisition.clone();
            settings.band = detail.band;
            settings.sweep_seconds = detail.seconds;
            settings.lf_detail = None;
            pair_duration(&settings, rate)
        } else {
            0.0
        };
        let moves = count - 1 + usize::from(prefs.layout.order[0] != prefs.layout.reference);
        let movement = match prefs.progression {
            Progression::Manual => None,
            Progression::HandsFree { movement_seconds } => {
                Some(Duration::from_secs_f64(movement_seconds * moves as f64))
            }
        };
        Ok(Self {
            acquisition: Duration::from_secs_f64(
                count as f64 * (prefs.acquisition.noise_seconds + full + lf) + full,
            ),
            moves,
            movement,
        })
    }
}
pub fn repeatability(a: &[Curve; 2], b: &[Curve; 2], band: Band) -> f64 {
    let mut sum = 0.0;
    let mut count = 0;
    for c in 0..2 {
        for ((&a, &b), &hz) in a[c].db.iter().zip(&b[c].db).zip(&a[c].frequency) {
            if band.contains(f64::from(hz)) {
                sum += f64::from(a - b).powi(2);
                count += 1;
            }
        }
    }
    assert!(
        count > 0,
        "repeatability needs samples in the comparison band"
    );
    (sum / count as f64).sqrt()
}
fn covers_objective(frozen: &Frozen, source: &Source, takes: &[Take; 2]) -> bool {
    takes.iter().enumerate().all(|(c, take)| {
        let original = source.points[take.position].full[c].response.trusted_band;
        frozen.target.frequency.iter().all(|&hz| {
            let hz = f64::from(hz);
            !frozen.recipe.range.contains(hz)
                || !original.contains(hz)
                || take.response.trusted_band.contains(hz)
        })
    })
}
fn apply_evidence(metrics: &mut Metrics, frozen: &Frozen, takes: &[Take; 2]) {
    metrics.noise_uncertainty_db = metrics.noise_uncertainty_db.max(noise_uncertainty(
        &takes.each_ref().map(|t| t.response.uncertainty_db.clone()),
        &frozen.target.frequency,
        frozen.recipe.range,
    ));
    for (c, take) in takes.iter().enumerate() {
        let requested = frozen
            .target
            .frequency
            .iter()
            .filter(|&&hz| frozen.recipe.range.contains(f64::from(hz)))
            .count();
        let supported = frozen
            .target
            .frequency
            .iter()
            .enumerate()
            .filter(|&(_, &hz)| {
                frozen.recipe.range.contains(f64::from(hz))
                    && take.response.trusted_band.contains(f64::from(hz))
            })
            .count();
        metrics.coverage[c] = metrics.coverage[c].min(supported as f64 / requested as f64);
    }
}
fn alignment_error(frozen: &Frozen, source: &Source, takes: &[Take; 2]) -> f64 {
    if !matches!(
        frozen.recipe.alignment,
        room::Alignment::Time | room::Alignment::LevelAndTime
    ) {
        return 0.0;
    }
    let original = &source.points[takes[0].position].full;
    let before = (original[1].response.arrival_frames - original[0].response.arrival_frames)
        / f64::from(source.plan.rate.hz());
    let expected = before + frozen.alignment.delay_seconds[1] - frozen.alignment.delay_seconds[0];
    let measured = (takes[1].response.arrival_frames - takes[0].response.arrival_frames)
        / f64::from(takes[0].probe.rate.hz());
    (measured - expected).abs() * 1000.0
}
fn noise_uncertainty(values: &[Vec<f32>; 2], frequencies: &[f32], band: Band) -> f64 {
    values
        .iter()
        .map(|values| {
            let mut sum = 0.0;
            let mut count = 0;
            for (i, &value) in values.iter().enumerate() {
                if band.contains(f64::from(frequencies[i])) {
                    sum += f64::from(value).powi(2);
                    count += 1;
                }
            }
            if count == 0 {
                f64::INFINITY
            } else {
                (sum / count as f64).sqrt()
            }
        })
        .fold(0.0, f64::max)
}
pub fn metrics(
    frozen: &Frozen,
    source: &Source,
    curves: &[Curve; 2],
    repeatability_db: f64,
    band: Option<Band>,
) -> Result<Metrics> {
    let mut m = Metrics {
        processed_rms_db: [0.0; 2],
        requested_rms_db: [0.0; 2],
        target_rms_db: [0.0; 2],
        percentile90_db: [0.0; 2],
        coverage: [0.0; 2],
        repeatability_db,
        noise_uncertainty_db: noise_uncertainty(
            &frozen.baseline.uncertainty_db,
            &frozen.target.frequency,
            frozen.recipe.range,
        ),
        unexpected_outside_fit_db: 0.0,
        alignment_error_ms: alignment_error(
            frozen,
            source,
            &source.points[source.plan.layout.reference].full,
        ),
        per_position_rms: Vec::new(),
    };
    let reference = source.plan.layout.reference;
    for (c, curve) in curves.iter().enumerate() {
        let mut errors = Vec::new();
        let mut count = 0;
        let mut requested = 0;
        let mut processed = 0.0;
        let mut effort = 0.0;
        let mut target = 0.0;
        for (i, &hz) in frozen.target.frequency.iter().enumerate() {
            if !frozen.recipe.range.contains(f64::from(hz))
                || band.is_some_and(|b| !b.contains(f64::from(hz)))
            {
                continue;
            }
            requested += 1;
            let reference_offset = source.points[reference].full[c].response.magnitude.db[i]
                + frozen.alignment.trim_db[c] as f32
                - frozen.baseline.magnitude[c].db[i];
            let p = f64::from(curve.db[i] - frozen.processed_goal[c].db[i] - reference_offset);
            let r = f64::from(curve.db[i] - frozen.requested_goal[c].db[i] - reference_offset);
            let t = f64::from(curve.db[i] - frozen.target.db[i]);
            processed += p * p;
            effort += r * r;
            target += t * t;
            errors.push(p.abs());
            count += 1;
        }
        if count == 0 {
            return Err(invalid("No comparable trusted bands"));
        }
        errors.sort_by(f64::total_cmp);
        m.processed_rms_db[c] = (processed / count as f64).sqrt();
        m.requested_rms_db[c] = (effort / count as f64).sqrt();
        m.target_rms_db[c] = (target / count as f64).sqrt();
        m.percentile90_db[c] = errors[(errors.len() * 9 / 10).min(errors.len() - 1)];
        m.coverage[c] = count as f64 / requested as f64;
    }
    Ok(m)
}
fn spatial_metrics(
    frozen: &Frozen,
    source: &Source,
    takes: &[[Take; 2]],
    repeat: f64,
) -> Result<Metrics> {
    assert!(!takes.is_empty(), "spatial metrics need recorded positions");
    let reference = source.plan.layout.reference;
    let mut result = None::<Metrics>;
    let mut total_weight = 0.0;
    let mut per_position = Vec::new();
    for pair in takes {
        let position = pair[0].position;
        if pair[1].position != position || position >= source.points.len() {
            return Err(invalid("Spatial pair has inconsistent positions"));
        }
        let equivalent = std::array::from_fn(|c| Curve {
            frequency: pair[c].response.magnitude.frequency.clone(),
            db: pair[c]
                .response
                .magnitude
                .db
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    v - source.points[position].full[c].response.magnitude.db[i]
                        + source.points[reference].full[c].response.magnitude.db[i]
                })
                .collect(),
        });
        let mut m = metrics(frozen, source, &equivalent, repeat, None)?;
        apply_evidence(&mut m, frozen, pair);

        for (c, take) in pair.iter().enumerate() {
            let mut sum = 0.0;
            let mut count = 0;
            for (i, &hz) in frozen.target.frequency.iter().enumerate() {
                if frozen.recipe.range.contains(f64::from(hz)) {
                    sum += f64::from(take.response.magnitude.db[i] - frozen.target.db[i]).powi(2);
                    count += 1;
                }
            }
            m.target_rms_db[c] = (sum / count as f64).sqrt();
        }
        m.alignment_error_ms = alignment_error(frozen, source, pair);
        per_position.push(m.processed_rms_db);
        let weight = frozen.baseline.weights[position];
        total_weight += weight;
        let aggregate = result.get_or_insert_with(|| Metrics {
            processed_rms_db: [0.0; 2],
            requested_rms_db: [0.0; 2],
            target_rms_db: [0.0; 2],
            percentile90_db: [0.0; 2],
            coverage: [1.0; 2],
            repeatability_db: repeat,
            noise_uncertainty_db: repeat,
            unexpected_outside_fit_db: 0.0,
            alignment_error_ms: 0.0,
            per_position_rms: Vec::new(),
        });
        aggregate.noise_uncertainty_db = aggregate.noise_uncertainty_db.max(m.noise_uncertainty_db);
        aggregate.alignment_error_ms = aggregate.alignment_error_ms.max(m.alignment_error_ms);
        for c in 0..2 {
            aggregate.processed_rms_db[c] += weight * m.processed_rms_db[c].powi(2);
            aggregate.requested_rms_db[c] += weight * m.requested_rms_db[c].powi(2);
            aggregate.target_rms_db[c] += weight * m.target_rms_db[c].powi(2);
            aggregate.percentile90_db[c] = aggregate.percentile90_db[c].max(m.percentile90_db[c]);
            aggregate.coverage[c] = aggregate.coverage[c].min(m.coverage[c]);
        }
    }
    let mut result = result.expect("nonempty spatial evidence");
    for c in 0..2 {
        result.processed_rms_db[c] = (result.processed_rms_db[c] / total_weight).sqrt();
        result.requested_rms_db[c] = (result.requested_rms_db[c] / total_weight).sqrt();
        result.target_rms_db[c] = (result.target_rms_db[c] / total_weight).sqrt();
    }
    result.per_position_rms = per_position;
    Ok(result)
}
pub fn changed_bands(before: &[Vec<f32>; 2], after: &[Vec<f32>; 2], f: &[f32]) -> Vec<Band> {
    let mut bands = Vec::new();
    let mut start = None;
    for i in 0..f.len() {
        let changed = (0..2).any(|c| (after[c][i] - before[c][i]).abs() > 0.1);
        match (start, changed) {
            (None, true) => start = Some(i),
            (Some(s), false) => {
                if i > s {
                    bands.push(Band {
                        low: f64::from(f[s]),
                        high: f64::from(f[i]),
                    });
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start
        && s + 1 < f.len()
    {
        bands.push(Band {
            low: f64::from(f[s]),
            high: f64::from(*f.last().expect("grid")),
        });
    }
    bands
}

/// Offline edits append a new immutable, explicitly unverified descendant.
pub fn prepare(
    session_id: ContentHash,
    recipe: Recipe,
    parent: Option<PassId>,
    store: &Store,
) -> Result<(ContentHash, Pass, Correction)> {
    let source = Source::load(Session::load(session_id, store)?.source, store)?;
    prepare_at_rate(session_id, recipe, parent, source.plan.rate, store)
}
pub fn prepare_at_rate(
    session_id: ContentHash,
    recipe: Recipe,
    parent: Option<PassId>,
    rate: Rate,
    store: &Store,
) -> Result<(ContentHash, Pass, Correction)> {
    let mut session = Session::load(session_id, store)?;
    let source = Source::load(session.source, store)?;
    let frozen = room::freeze_at_rate(&source, session.source, recipe, rate)?;
    let objective = store.put(&serde_json::to_vec(&frozen)?)?;
    let (correction, realized) = room::render(
        &frozen,
        &frozen.designed_db,
        rate,
        "Room correction / edited",
    )?;
    let id = store.put(&serde_json::to_vec(&correction)?)?;
    let reference = source.plan.layout.reference;
    let actual = correction.response(481);
    let predicted = std::array::from_fn(|c| Curve {
        frequency: frozen.target.frequency.clone(),
        db: source.points[reference].full[c]
            .response
            .magnitude
            .frequency
            .iter()
            .enumerate()
            .map(|(i, &f)| {
                source.points[reference].full[c].response.magnitude.db[i] + actual[c].at(f)
            })
            .collect(),
    });
    let history = session.history(store)?;
    if parent.is_some_and(|p| p.0 >= history.len()) {
        return Err(invalid("Unknown parent pass"));
    }
    let prior = parent
        .and_then(|p| match &history[p.0].artifact {
            Artifact::Candidate { design_db, .. } => Some(design_db.clone()),
            Artifact::Baseline => None,
        })
        .unwrap_or_else(|| std::array::from_fn(|_| vec![0.0; frozen.target.db.len()]));
    let trial = Trial {
        parent,
        objective,
        artifact: Artifact::Candidate {
            correction: id,
            design_db: frozen.designed_db.clone(),
            realized,
        },
        changed: changed_bands(&prior, &frozen.designed_db, &frozen.target.frequency),
        decision: Decision::Prepared,
        scope: Scope::Prediction {
            band: source.points[reference].full[0].response.trusted_band,
        },
        metrics: metrics(&frozen, &source, &predicted, 0.0, None)?,
        measured: Vec::new(),
    };
    let pass = Pass {
        id: PassId(session.passes.len()),
        context: Context::Offline,
        trial,
    };
    session.passes.push(store.put(&serde_json::to_vec(&pass)?)?);
    Ok((session.save(store)?, pass, correction))
}

/// Export the manifest and its transitive managed objects without rewriting evidence.
pub fn export_session(id: ContentHash, destination: &std::path::Path, store: &Store) -> Result<()> {
    let session = Session::load(id, store)?;
    let source = Source::load(session.source, store)?;
    let mut objects = std::collections::BTreeSet::from([id, session.source]);
    objects.extend(source.referenced_objects());
    objects.extend(&session.passes);
    for pass in session.history(store)? {
        objects.insert(pass.objective);
        if let Context::Measured { settings, .. } = pass.context {
            objects.insert(settings);
        }
        if let Some(correction) = pass.correction() {
            objects.insert(correction);
        }
        for take in pass.measured.iter().flatten() {
            objects.extend(take.referenced_objects());
        }
    }
    std::fs::create_dir_all(destination.join("objects"))?;
    if matches!(
        source.plan.route.playback.uid,
        crate::domain::DeviceUid::Emulated { .. }
    ) {
        let configuration = store.root().join("simulation.json");
        if configuration.exists() {
            crate::storage::atomic_write(
                &destination.join("simulation.json"),
                &std::fs::read(configuration)?,
            )?;
        }
    }
    for hash in objects {
        let bytes = store.get(&hash)?;
        crate::storage::atomic_write(&destination.join("objects").join(hash.as_str()), &bytes)?;
    }
    for point in &source.points {
        let noise_text = format!(
            "# Unweighted microphone noise PSD / dBFS per Hz; RMS {:.3} dBFS; window variation {:.3} dB\n{}",
            point.noise.rms_dbfs,
            point.noise.window_variation_db,
            point
                .noise
                .psd_dbfs_per_hz
                .frequency
                .iter()
                .zip(&point.noise.psd_dbfs_per_hz.db)
                .map(|(hz, db)| format!("{hz:.6}\t{db:.6}\n"))
                .collect::<String>()
        );
        crate::storage::atomic_write(
            &destination.join(format!("point-{:03}-noise.txt", point.position + 1)),
            noise_text.as_bytes(),
        )?;
        for (kind, takes) in
            std::iter::once(("full", &point.full)).chain(point.lf.as_ref().map(|lf| ("lf", lf)))
        {
            for take in takes {
                let speaker = match take.speaker {
                    Speaker::Left => "left",
                    Speaker::Right => "right",
                };
                let name = format!("point-{:03}-{speaker}-{kind}", point.position + 1);
                let impulse = take.response.impulse.load(store)?;
                crate::storage::atomic_write(
                    &destination.join(format!("{name}-raw-transfer-ir.wav")),
                    &float_wav(take.response.impulse.rate, &[&impulse])?,
                )?;
                let capture = take.raw.load(store)?;
                crate::storage::atomic_write(
                    &destination.join(format!("{name}-original-mic.wav")),
                    &float_wav(take.raw.rate, &[&capture])?,
                )?;
                crate::storage::atomic_write(
                    &destination.join(format!("{name}.measurement.json")),
                    &serde_json::to_vec_pretty(take)?,
                )?;
                let mut response = format!(
                    "# Calibrated relative acoustic magnitude dB; no SPL inferred\n# Trusted interior {:.6}-{:.6} Hz; IR zero is {} frames after its start, relative to fixed left reference\n# IR WAV retains microphone response; text applies microphone magnitude calibration\n# Hz\tdB\tuncertainty_dB\n",
                    take.response.trusted_band.low,
                    take.response.trusted_band.high,
                    take.response.impulse.rate.hz() / 50
                );
                for (i, &hz) in take.response.magnitude.frequency.iter().enumerate() {
                    response.push_str(&format!(
                        "{hz:.6}\t{:.6}\t{:.6}\n",
                        take.response.magnitude.db[i], take.response.uncertainty_db[i]
                    ));
                }
                crate::storage::atomic_write(
                    &destination.join(format!("{name}-response.txt")),
                    response.as_bytes(),
                )?;
            }
        }
    }
    crate::storage::atomic_write(
        &destination.join("session.json"),
        &serde_json::to_vec_pretty(&id)?,
    )
}

pub fn export_filter(id: ContentHash, path: &std::path::Path, store: &Store) -> Result<()> {
    let correction: Correction = serde_json::from_slice(&store.get(&id)?)?;
    correction.validate()?;
    let generation = match correction.origin {
        crate::correction::Origin::Room { objective, .. } => {
            ExportGeneration::Room(Box::new(serde_json::from_slice(&store.get(&objective)?)?))
        }
        _ => ExportGeneration::ExactStored,
    };
    let recipe = FilterExport {
        version: 1,
        channel_order: Speaker::ALL,
        gain_convention: "FIR gain, speaker trims and delays are included. Listening volume, comparison gains and playback headroom are excluded.",
        filter: &correction,
        generation,
    };
    crate::storage::atomic_write(
        path,
        &float_wav(
            correction.rate,
            &correction.channels.each_ref().map(Vec::as_slice),
        )?,
    )?;
    crate::storage::atomic_write(
        &path.with_extension("recipe.json"),
        &serde_json::to_vec_pretty(&recipe)?,
    )
}
#[derive(Serialize)]
struct FilterExport<'a> {
    version: u32,
    channel_order: [Speaker; 2],
    gain_convention: &'static str,
    filter: &'a Correction,
    generation: ExportGeneration,
}
#[derive(Serialize)]
enum ExportGeneration {
    Room(Box<Frozen>),
    ExactStored,
}
fn float_wav(rate: Rate, channels: &[&[f32]]) -> Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: u16::try_from(channels.len()).expect("one or two exported channels"),
        sample_rate: rate.hz(),
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut bytes = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut bytes, spec)?;
        let frames = channels.iter().map(|ch| ch.len()).max().expect("stereo");
        for i in 0..frames {
            for channel in channels {
                writer.write_sample(channel.get(i).copied().unwrap_or(0.0))?;
            }
        }
        writer.finalize()?;
    }
    Ok(bytes.into_inner())
}

#[cfg(test)]
mod persistence_contract_tests {
    use super::*;

    #[test]
    fn candidate_reopening_and_preference_rebasing_preserve_verified_history() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::at(directory.path()).unwrap();
        let mut source = simulation::plan(Rate::Hz48000);
        source.acquisition.sweep_seconds = 1.0;
        source.acquisition.tail_seconds = 1.0;
        source.acquisition.noise_seconds = 1.0;
        let (_tx, controls) = std::sync::mpsc::sync_channel(8);
        let recipe = Recipe {
            target: room::Target::Flat,
            maximum_boost_db: Some(3.0),
            maximum_cut_db: Some(12.0),
            effort: room::Effort::easy(0.5),
            taps: room::Taps::Short,
            ..Recipe::default()
        };
        let id = Run {
            plan: Plan {
                levels: None,
                source,
                recipe: recipe.clone(),
                progression: Progression::Manual,
                kind: RunKind::Measure,
                session: None,
            },
            store: &store,
            cancel: crate::audio::Cancellation::new(),
            controls,
            progress: Box::new(|_| {}),
            provider: Box::new(
                simulation::Simulated::new(simulation::Scenario::default()).unwrap(),
            ),
        }
        .execute()
        .unwrap();
        let mut current = Session::load(id, &store).unwrap();
        let history = current.history(&store).unwrap();
        let best = current
            .default_candidate(&history)
            .expect("verified correction");
        assert_eq!(history[best.0].decision, Decision::Prepared);

        let (draft_id, draft, _) = prepare(id, recipe.clone(), Some(best), &store).unwrap();
        let mut prepared = Session::load(draft_id, &store).unwrap();
        assert_eq!(
            prepared.default_candidate(&prepared.history(&store).unwrap()),
            Some(draft.id)
        );
        current.prefer(best, &store).unwrap();
        prepared.rebase_preference(&current).unwrap();
        assert_eq!(prepared.preferred, Some(best));
        assert_eq!(prepared.best, current.best);

        let mut rejected = draft;
        rejected.trial.decision = Decision::Rejected;
        let last = prepared.passes.last_mut().unwrap();
        *last = store.put(&serde_json::to_vec(&rejected).unwrap()).unwrap();
        let reopened = Session::load(prepared.save(&store).unwrap(), &store).unwrap();
        assert_eq!(
            reopened.default_candidate(&reopened.history(&store).unwrap()),
            Some(best)
        );

        let mut planned = Recovery::Session(id).source_plan(&store).unwrap();
        planned.layout.positions[0].name = "Renamed reference".into();
        planned.layout.positions[0].coordinates_cm = Some([15.0, 20.0, 0.0]);
        planned.layout.positions.push(Position {
            name: "Future point".into(),
            coordinates_cm: Some([50.0, 0.0, 0.0]),
            weight: 1.0,
        });
        planned.layout.order = vec![1, 0];
        planned.layout.reference = 1;
        planned.acquisition.noise_limits.maximum_variation_db = 12.5;
        planned.acquisition.noise_limits.maximum_crest_db = 30.0;
        let (tx, controls) = std::sync::mpsc::sync_channel(8);
        let verified = Run {
            plan: Plan {
                levels: None,
                source: planned,
                recipe,
                progression: Progression::Manual,
                kind: RunKind::Verify { pass: best },
                session: Some(id),
            },
            store: &store,
            cancel: crate::audio::Cancellation::new(),
            controls,
            progress: Box::new(move |progress| {
                if let Progress::Noise { ref noise, .. } = progress {
                    assert_eq!(noise.limits.maximum_variation_db, 12.5);
                    assert_eq!(noise.limits.maximum_crest_db, 30.0);
                }
                if let Progress::Move { movement, .. } = progress {
                    assert_eq!(movement.position(), 0);
                    tx.send(Control {
                        movement,
                        action: MoveAction::Ready,
                    })
                    .unwrap();
                }
            }),
            provider: Box::new(
                simulation::Simulated::new(simulation::Scenario::default()).unwrap(),
            ),
        }
        .execute()
        .unwrap();
        assert_eq!(
            Session::load(verified, &store).unwrap().source,
            current.source
        );
        assert!(
            prepared
                .rebase_preference(&Session {
                    source: ContentHash::of(b"another source"),
                    ..current
                })
                .is_err()
        );
    }
}
