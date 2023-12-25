use std::{
    process::{Child, Command},
    rc::Rc,
    str::FromStr,
    time::Duration,
};

use indexmap::IndexMap;
use nonblock::NonBlockingReader;
use once_cell::sync::Lazy;
use regex::Regex;
use tempfile::NamedTempFile;

use crate::common::TaskSetCpuList;

pub trait ProcessRunner: ::std::fmt::Debug {
    type Command;

    fn run(
        &self,
        command: &Self::Command,
        vcpus: &TaskSetCpuList,
        tmp_file: &mut NamedTempFile,
    ) -> anyhow::Result<Child>;
    fn info(&self) -> String;
    fn keys(&self) -> IndexMap<String, String>;
}

#[derive(Debug)]
pub struct RunConfig<C> {
    pub server_runner: Rc<dyn ProcessRunner<Command = C>>,
    pub server_vcpus: TaskSetCpuList,
    pub load_test_runner: Box<dyn ProcessRunner<Command = C>>,
    pub load_test_vcpus: TaskSetCpuList,
}

impl<C> RunConfig<C> {
    pub fn run(self, command: &C) -> Result<RunResults<C>, RunResults<C>> {
        let mut server_config_file = NamedTempFile::new().unwrap();
        let mut load_test_config_file = NamedTempFile::new().unwrap();

        let server =
            match self
                .server_runner
                .run(command, &self.server_vcpus, &mut server_config_file)
            {
                Ok(handle) => ChildWrapper(handle),
                Err(err) => return Err(RunResults::new(self).set_error(err.into(), "run server")),
            };

        ::std::thread::sleep(Duration::from_secs(1));

        let mut load_tester = match self.load_test_runner.run(
            command,
            &self.load_test_vcpus,
            &mut load_test_config_file,
        ) {
            Ok(handle) => ChildWrapper(handle),
            Err(err) => {
                return Err(RunResults::new(self)
                    .set_error(err.into(), "run load test")
                    .set_server(server))
            }
        };

        ::std::thread::sleep(Duration::from_secs(59));

        let cpu_stats_res = Command::new("ps")
            .arg("-p")
            .arg(server.0.id().to_string())
            .arg("-o")
            .arg("%cpu,rss")
            .arg("--noheader")
            .output();

        let server_process_stats = match cpu_stats_res {
            Ok(output) if output.status.success() => {
                ProcessStats::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap()
            }
            Ok(_) => {
                return Err(RunResults::new(self)
                    .set_error_context("run ps")
                    .set_server(server)
                    .set_load_test(load_tester));
            }
            Err(err) => {
                return Err(RunResults::new(self)
                    .set_error(err.into(), "run ps")
                    .set_server(server)
                    .set_load_test(load_tester));
            }
        };

        ::std::thread::sleep(Duration::from_secs(5));

        let load_test_data = match load_tester.0.try_wait() {
            Ok(Some(status)) if status.success() => read_child_outputs(load_tester),
            Ok(Some(_)) => {
                return Err(RunResults::new(self)
                    .set_error_context("wait for load tester")
                    .set_server(server)
                    .set_load_test(load_tester))
            }
            Ok(None) => {
                if let Err(err) = load_tester.0.kill() {
                    return Err(RunResults::new(self)
                        .set_error(err.into(), "kill load tester")
                        .set_server(server)
                        .set_load_test(load_tester));
                }

                ::std::thread::sleep(Duration::from_secs(1));

                match load_tester.0.try_wait() {
                    Ok(_) => {
                        return Err(RunResults::new(self)
                            .set_error_context("load tester didn't finish in time")
                            .set_load_test(load_tester))
                    }
                    Err(err) => {
                        return Err(RunResults::new(self)
                            .set_error(err.into(), "wait for load tester after kill")
                            .set_server(server));
                    }
                }
            }
            Err(err) => {
                return Err(RunResults::new(self)
                    .set_error(err.into(), "wait for load tester")
                    .set_server(server)
                    .set_load_test(load_tester))
            }
        };

        let mut results = RunResults::new(self);

        results.server_process_stats = Some(server_process_stats);
        results.load_test_stdout = load_test_data.0;
        results.load_test_stderr = load_test_data.1;

        Ok(results)
    }
}

#[derive(Debug)]
pub struct RunResults<C> {
    pub run_config: RunConfig<C>,
    pub server_process_stats: Option<ProcessStats>,
    pub server_stdout: Option<String>,
    pub server_stderr: Option<String>,
    pub load_test_stdout: Option<String>,
    pub load_test_stderr: Option<String>,
    pub error: Option<anyhow::Error>,
    pub error_context: Option<String>,
}

impl<C> RunResults<C> {
    pub fn avg_responses(&self) -> Option<String> {
        static RE: Lazy<Regex> =
            Lazy::new(|| Regex::new(r"Average responses per second: ([0-9]+\.?[0-9]+)").unwrap());

        self.load_test_stdout.as_ref().and_then(|stdout| {
            RE.captures_iter(&stdout).next().map(|c| {
                let (_, [avg_responses]) = c.extract();

                avg_responses.to_string()
            })
        })
    }

    fn new(run_config: RunConfig<C>) -> Self {
        Self {
            run_config,
            server_process_stats: Default::default(),
            server_stdout: Default::default(),
            server_stderr: Default::default(),
            load_test_stdout: Default::default(),
            load_test_stderr: Default::default(),
            error: Default::default(),
            error_context: Default::default(),
        }
    }

    fn set_server(mut self, server: ChildWrapper) -> Self {
        let (stdout, stderr) = read_child_outputs(server);

        self.server_stdout = stdout;
        self.server_stderr = stderr;

        self
    }

    fn set_load_test(mut self, load_test: ChildWrapper) -> Self {
        let (stdout, stderr) = read_child_outputs(load_test);

        self.load_test_stdout = stdout;
        self.load_test_stderr = stderr;

        self
    }

    fn set_error(mut self, error: anyhow::Error, context: &str) -> Self {
        self.error = Some(error);
        self.error_context = Some(context.to_string());

        self
    }

    fn set_error_context(mut self, context: &str) -> Self {
        self.error_context = Some(context.to_string());

        self
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ProcessStats {
    pub avg_cpu_utilization: f32,
    pub peak_rss_kb: f32,
}

impl FromStr for ProcessStats {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.trim().split_whitespace();

        Ok(Self {
            avg_cpu_utilization: parts.next().ok_or(())?.parse().map_err(|_| ())?,
            peak_rss_kb: parts.next().ok_or(())?.parse().map_err(|_| ())?,
        })
    }
}

struct ChildWrapper(Child);

impl Drop for ChildWrapper {
    fn drop(&mut self) {
        let _ = self.0.kill();

        ::std::thread::sleep(Duration::from_secs(1));

        let _ = self.0.try_wait();
    }
}

impl AsMut<Child> for ChildWrapper {
    fn as_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

fn read_child_outputs(mut child: ChildWrapper) -> (Option<String>, Option<String>) {
    let stdout = child.0.stdout.take().map(|stdout| {
        let mut buf = String::new();

        let mut reader = NonBlockingReader::from_fd(stdout).unwrap();

        reader.read_available_to_string(&mut buf).unwrap();

        buf
    });
    let stderr = child.0.stderr.take().map(|stderr| {
        let mut buf = String::new();

        let mut reader = NonBlockingReader::from_fd(stderr).unwrap();

        reader.read_available_to_string(&mut buf).unwrap();

        buf
    });

    (stdout, stderr)
}