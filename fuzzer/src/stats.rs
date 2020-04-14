use crate::corpus::Corpus;
use crate::feedback::FeedBack;
use crate::mail;
use crate::report::TestCaseRecord;
use crate::utils::queue::CQueue;
use lettre_email::EmailBuilder;

use circular_queue::CircularQueue;
use core::prog::Prog;
use std::sync::Arc;
use tokio::fs::write;
use tokio::sync::broadcast;
use tokio::time;
use tokio::time::Duration;
use std::process::exit;

pub struct StatSource {
    pub corpus: Arc<Corpus>,
    pub feedback: Arc<FeedBack>,
    pub candidates: Arc<CQueue<Prog>>,
    pub record: Arc<TestCaseRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub corpus: usize,
    pub blocks: usize,
    pub branches: usize,
    // pub exec:usize,
    // pub gen:usize,
    // pub minimized:usize,
    pub candidates: usize,
    pub normal_case: usize,
    pub failed_case: usize,
    pub crashed_case: usize,
}

#[derive(Debug, Deserialize)]
pub struct SamplerConf {
    /// Duration for sampling, per second
    pub sample_interval: u64,
    /// Duration for report, per minites
    pub report_interval: u64,
}

impl SamplerConf {
    pub fn check(&self) {
        if self.sample_interval < 10 || self.report_interval <= 10 ||
            self.sample_interval < report_interval * 60 {
            eprintln!("Config Error: invalid sample conf");
            exit(exitcode::CONFIG)
        }
    }
}

pub struct Sampler {
    pub source: StatSource,
    pub stats: CircularQueue<Stats>,
    pub shutdown: broadcast::Receiver<()>,
    pub work_dir: String,
}

impl Sampler {
    pub async fn sample(&mut self, conf: &Option<SamplerConf>) {
        let (sample_interval, report_interval) = match conf {
            Some(SamplerConf {
                     sample_interval,
                     report_interval,
                 }) => {
                assert!(*sample_interval < *report_interval * 60);
                (
                    Duration::new(*sample_interval, 0),
                    Duration::new(*report_interval * 60, 0),
                )
            }
            None => (Duration::new(15, 0), Duration::new(60 * 60, 0)),
        };

        use broadcast::TryRecvError::*;
        let mut last_report = Duration::new(0, 0);
        loop {
            match self.shutdown.try_recv() {
                Ok(_) => {
                    self.persist().await;
                    return;
                }
                Err(e) => match e {
                    Empty => (),
                    Closed | Lagged(_) => panic!("Unexpected braodcast receiver state"),
                },
            }

            time::delay_for(sample_interval).await;
            last_report += sample_interval;

            let (corpus, (blocks, branches), candidates, (normal_case, failed_case, crashed_case)) = tokio::join!(
                self.source.corpus.len(),
                self.source.feedback.len(),
                self.source.candidates.len(),
                self.source.record.len()
            );
            let stat = Stats {
                corpus,
                blocks,
                branches,
                candidates,
                normal_case,
                failed_case,
                crashed_case,
            };

            if report_interval <= last_report {
                self.report(&stat).await;
                last_report = Duration::new(0, 0);
            }

            self.stats.push(stat);
            info!("corpus {},blocks {},branches {},candidates {},normal_case {},failed_case {},crashed_case {}",
                  corpus, blocks, branches, candidates, normal_case, failed_case, crashed_case);
        }
    }

    async fn persist(&self) {
        if self.stats.is_empty() {
            return;
        }

        let stats = self.stats.asc_iter().cloned().collect::<Vec<_>>();
        let path = format!("{}/stats.json", self.work_dir);
        let stats = serde_json::to_string_pretty(&stats).unwrap();
        write(&path, stats).await.unwrap_or_else(|e| {
            exits!(exitcode::IOERR, "Fail to persist stats to {} : {}", path, e)
        })
    }

    async fn report(&self, stat: &Stats) {
        let stat = serde_json::to_string_pretty(&stat).unwrap();
        let email = EmailBuilder::new()
            .subject("Healer-Stats Regular Report")
            .body(stat);
        mail::send(email).await
    }
}
