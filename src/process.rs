use std::borrow::BorrowMut;
use std::io::Read;
use std::net;
use std::process::{Child, Command, Stdio};

use failure::{Error, Fail};
use log::*;
use rand::seq::SliceRandom;
use rand::thread_rng;
use regex::Regex;
use tempdir::TempDir;

use crate::helpers::{wait_for_mut, WaitOptions};

//use crate::page_session::PageSession;
//use crate::tab::Tab;

pub struct Process {
    child_process: Child,
    pub debug_ws_url: String,
}

#[derive(Debug, Fail)]
enum ChromeLaunchError {
    #[fail(display = "Chrome launched, but didn't give us a WebSocket URL before we timed out")]
    PortOpenTimeout,
    #[fail(display = "There are no available ports between 8000 and 9000 for debugging")]
    NoAvailablePorts,
    #[fail(display = "The chosen debugging port is already in use")]
    DebugPortInUse,
}

pub struct LaunchOptions<'a> {
    pub headless: bool,
    pub port: Option<u16>,
    pub path: &'a str
}

impl<'a> Default for LaunchOptions<'a> {
    fn default() -> Self {
        LaunchOptions {
            headless: true,
            // TODO: extra option for if you want it to keep scanning up from the port you passed?
            port: None,
            // TODO: this is not at all a sensible default
            path: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        }
    }
}

impl Process {
    // TODO: find out why this complains if named 'new'
    pub fn new(launch_options: LaunchOptions) -> Result<Self, Error> {
        info!("Trying to start Chrome");

        let mut process = Process::start_process(&launch_options)?;

        info!("Started Chrome. PID: {}", process.id());

        let url;
        let mut attempts = 0;
        loop {
            if attempts > 50 {
                return Err(ChromeLaunchError::NoAvailablePorts {}.into());
            }

            match Process::ws_url_from_output(process.borrow_mut()) {
                Ok(debug_ws_url) => {
                    url = debug_ws_url;
                    break;
                }
                Err(error) => {
                    if launch_options.port.is_none() {
                        process = Process::start_process(&launch_options)?;
                    } else {
                        return Err(error);
                    }
                }
            }

            trace!("Trying again to find available debugging port. Attempts: {}", attempts);
            attempts = attempts + 1;
        }

        Ok(Process {
            child_process: process,
            debug_ws_url: url,
        })
    }

    fn start_process(launch_options: &LaunchOptions) -> Result<Child, Error> {
        let debug_port = if let Some(port) = launch_options.port {
            port
        } else {
            get_available_port().ok_or(ChromeLaunchError::NoAvailablePorts {})?
        };
        let port_option = format!("--remote-debugging-port={}", debug_port);

        // NOTE: picking random data dir so that each a new browser instance is launched
        // (see man google-chrome)
        let user_data_dir = TempDir::new("rust-headless-chrome-profile")?;
        let data_dir_option = format!("--user-data-dir={}", user_data_dir.path().to_str().unwrap());

        trace!("Chrome will have profile: {}", data_dir_option);

        let mut args = vec![
            port_option.as_str(),
            "--verbose",
            "--no-first-run",
            data_dir_option.as_str(),
//            "--window-size=1920,1080"
        ];

        if launch_options.headless {
            args.extend(&["--headless"]);
        }

        let process = Command::new(launch_options.path)
            .args(&args)
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(process)
    }


    // TODO: URL instead of String return type?
    // let url = Url::parse("ws://bitcoins.pizza").unwrap();
    //
    // let builder = ClientBuilder::from_url(&url);
    fn ws_url_from_output(child_process: &mut Child) -> Result<String, Error> {
        // TODO: will this work on Mac / Windows / etc.?
        let port_taken_re = Regex::new(r"Address already in use").unwrap();

        // TODO: user static or lazy static regex
        let re = Regex::new(r"listening on (.*/devtools/browser/.*)\n").unwrap();

        let extract = |text: &str| -> Option<String> {
            let caps = re.captures(text);
            let cap = &caps?[1];
            Some(cap.into())
        };


        let chrome_output_result = wait_for_mut(|| {
            let mut buf = [0; 512];
            let my_stderr = child_process.stderr.as_mut();
            // TODO: actually handle this error
            let bytes_read = my_stderr.unwrap().read(&mut buf).unwrap();
            if bytes_read > 0 {
                let chrome_output = String::from_utf8_lossy(&buf);
                trace!("Chrome output: {}", chrome_output);

                if port_taken_re.is_match(&chrome_output) {
                    return None;
                }

                extract(&chrome_output)
            } else {
                None
            }
        }, WaitOptions { timeout_ms: 200, sleep_ms: 10 });

        if let Ok(output) = chrome_output_result {
            if port_taken_re.is_match(&output) {
                Err(ChromeLaunchError::DebugPortInUse {}.into())
            } else {
                Ok(output)
            }
        } else {
            Err(ChromeLaunchError::PortOpenTimeout {}.into())
        }
    }

}

impl Drop for Process {
    fn drop(&mut self) {
        info!("Killing Chrome. PID: {}", self.child_process.id());
        self.child_process.kill().unwrap();
        self.child_process.wait().unwrap();
    }
}

fn get_available_port() -> Option<u16> {
    let mut ports: Vec<u16> = (8000..9000).collect();
    ports.shuffle(&mut thread_rng());
    ports.iter().find(|port| port_is_available(**port)).map(|p| p.clone())
}

fn port_is_available(port: u16) -> bool {
    net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}


#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::prelude::*;
    use std::thread;

    fn current_child_pids() -> Vec<i32> {
        let current_pid = std::process::id();
        let mut current_process_children_file = File::open(format!("/proc/{}/task/{}/children", current_pid, current_pid)).unwrap();
        let mut child_pids = String::new();
        current_process_children_file.read_to_string(&mut child_pids).unwrap();
        return child_pids.split_whitespace().map(|pid_str| pid_str.parse::<i32>().unwrap()).collect();
    }

    #[test]
    fn kills_process_on_drop() {
        env_logger::try_init().unwrap_or(());
        let time_before = std::time::SystemTime::now();
        {
            let _chrome = &mut super::Process::new(Default::default()).unwrap();
        }

        let child_pids = current_child_pids();
        assert!(child_pids.is_empty());
    }

    #[test]
    fn launch_multiple_non_headless_instances() {
        env_logger::try_init().unwrap_or(());

        let mut handles = Vec::new();

        for _ in 0..10 {
            let handle = thread::spawn(|| {
                // these sleeps are to make it more likely the chrome startups will overlap
                std::thread::sleep(std::time::Duration::from_millis(10));
                let chrome = super::Process::new(super::LaunchOptions {
                    port: None,
                    ..Default::default()
                }).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(100));
                chrome.debug_ws_url.clone()
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }
    }


    #[test]
    fn no_instance_sharing() {
        env_logger::try_init().unwrap_or(());

        let mut handles = Vec::new();

        for _ in 0..10 {
            let chrome = super::Process::new(super::LaunchOptions {
                headless: false,
                ..Default::default()
            }).unwrap();
            handles.push(chrome);
        };
    }
}