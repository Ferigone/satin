//! Test-only transport. It never creates a worker, client, or Restart.
use super::*;

type ResultSender = Sender<Result<Outcome, UpdateFailure>>;

#[derive(Default)]
pub(super) struct Transport {
    reply: Option<ResultSender>,
    commands: [usize; 3], // Check, Download, Restart
}

fn channel() -> (ResultSender, Job) {
    let (sender, result) = mpsc::channel();
    let job = Job {
        result,
        bytes: Arc::new(AtomicU64::new(0)),
        shown: 0,
        cancel: None,
        worker: None,
    };
    (sender, job)
}

impl Updates {
    pub(crate) fn ui_qa_fixture(profile: &Path) -> Self {
        let (reply, job) = channel();
        // A pending in-memory discovery result exercises the production poll
        // transition without executing a Check, including at startup.
        Self {
            install: Installation {
                root: profile.join("not-an-install"),
                running: profile.join("not-an-executable"),
                profile: profile.to_owned(),
            },
            wake: Arc::new(|| {}),
            phase: Phase::Checking,
            job: Some(job),
            ui_qa: Some(Transport {
                reply: Some(reply),
                ..Default::default()
            }),
        }
    }

    pub(super) fn begin_ui_qa_job(&mut self, action: UpdateAction) {
        let candidate = self.offered();
        let transport = self.ui_qa.as_mut().expect("offline fixture transport");
        match action {
            UpdateAction::Check => transport.commands[0] += 1,
            UpdateAction::Download => {
                transport.commands[1] += 1;
                let (reply, job) = channel();
                transport.reply = Some(reply);
                self.job = Some(job);
                self.phase = Phase::Downloading(candidate.expect("fixture offer"));
            }
            // Record the fully routed action; don't issue or simulate a
            // Prepared(Restart) outcome or change installation state.
            UpdateAction::Restart => transport.commands[2] += 1,
        }
    }

    pub(crate) fn ui_qa_offer(&mut self) -> Result<(), String> {
        self.ui_qa_reply(Outcome::Discovered(Some(Candidate {
            version: Version::new(99, 0, 0),
            size: 8 * 1024 * 1024,
            hash: [0; 32],
            url: Url::parse("https://unused.invalid/").expect("fixed fixture URL"),
        })))
    }

    pub(crate) fn ui_qa_progress(&self) -> Result<(), String> {
        let job = self.job.as_ref().ok_or("Missing in-memory transfer")?;
        job.bytes.store(3 * 1024 * 1024, Relaxed);
        Ok(())
    }

    pub(crate) fn ui_qa_ready(&mut self) -> Result<(), String> {
        self.ui_qa_reply(Outcome::Staged)
    }

    fn ui_qa_reply(&mut self, outcome: Outcome) -> Result<(), String> {
        self.ui_qa
            .as_mut()
            .and_then(|transport| transport.reply.take())
            .ok_or("Missing fixture result sender")?
            .send(Ok(outcome))
            .map_err(|_| "Fixture result receiver disconnected".into())
    }

    pub(crate) fn ui_qa_commands(&self) -> [usize; 3] {
        self.ui_qa.as_ref().expect("offline fixture transport").commands
    }
}
