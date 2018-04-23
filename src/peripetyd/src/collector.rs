extern crate nix;
extern crate peripety;
extern crate sdjournal;

// Collector is supposed to get log from systemd journal and generate
// event with kdev and sub system type.

// Many code are copied from Tony's
// https://github.com/tasleson/storage_event_monitor/blob/master/src/main.rs
// Which is MPL license.

use std::sync::mpsc::Sender;
use peripety::{LogSeverity, StorageEvent, StorageSubSystem};
use std::collections::HashMap;
use nix::sys::select::FdSet;
use std::os::unix::io::AsRawFd;
use data::{RegexConf, BUILD_IN_REGEX_CONFS};

fn process_journal_entry(
    entry: &HashMap<String, String>,
    sender: &Sender<StorageEvent>,
    regex_confs: &Vec<RegexConf>,
) {
    if !entry.contains_key("MESSAGE") {
        return;
    }

    if !entry.contains_key("SYSLOG_IDENTIFIER") {
        return;
    }

    // The /dev/kmsg can hold userspace log, hence using `_TRANSPORT=kernel` is
    // not correct here.
    if entry.get("SYSLOG_IDENTIFIER").unwrap() != "kernel" {
        return;
    }

    let mut event: StorageEvent = Default::default();

    // Currently, SCSI layer have limited structured log holding
    // device and subsystem type, but without regex, we cannot know the
    // event type. Hence we do regex anyway without checking structured log,
    // we can do that when kernel provide better structured log.
    if entry.contains_key("_KERNEL_SUBSYSTEM")
        && entry.contains_key("_KERNEL_DEVICE")
    {
        event.sub_system =
            entry.get("_KERNEL_SUBSYSTEM").unwrap().parse().unwrap();
        event.kdev = entry.get("_KERNEL_DEVICE").unwrap().to_string();
    }
    for regex_conf in regex_confs {
        let msg = entry.get("MESSAGE").unwrap();
        // Save CPU from regex.captures() if starts_with() failed.
        if regex_conf.starts_with.len() != 0
            && !msg.starts_with(&regex_conf.starts_with)
        {
            continue;
        }
        if let Some(cap) = regex_conf.regex.captures(msg) {
            if let Some(m) = cap.name("kdev") {
                event.kdev = m.as_str().to_string();
            }
            if event.kdev.len() == 0 {
                continue;
            }

            if regex_conf.sub_system != StorageSubSystem::Unknown {
                event.sub_system = regex_conf.sub_system;
            } else if let Some(m) = cap.name("sub_system") {
                event.sub_system = m.as_str().to_string().parse().unwrap();
            }

            if event.sub_system == StorageSubSystem::Unknown {
                continue;
            }

            if regex_conf.event_type.len() != 0 {
                event.event_type = regex_conf.event_type.to_string();
            }
            break;
        }
    }
    if event.sub_system == StorageSubSystem::Unknown || event.kdev.len() == 0 {
        return;
    }

    // Add other data
    event.hostname = entry
        .get("_HOSTNAME")
        .unwrap_or(&"".to_string())
        .to_string();
    event.timestamp = entry
        .get("__REALTIME_TIMESTAMP")
        .unwrap_or(&"0".to_string())
        .to_string()
        .parse()
        .unwrap();

    event.severity = entry
        .get("PRIORITY")
        .map(|m| m.parse::<LogSeverity>().unwrap())
        .unwrap();

    event.msg = entry.get("MESSAGE").unwrap().to_string();
    //TODO(Gris Ge): Generate event_id here.

    //TODO(Gris Ge): Need to skip journal entry when that one is created by
    //               peripety.
    sender.send(event).unwrap();
}

pub fn new(sender: &Sender<StorageEvent>) {
    let mut journal =
        sdjournal::Journal::new().expect("Failed to open systemd journal");
    // We never want to block, so set the timeout to 0
    journal.timeout_us = 0;
    // Jump to the end as we cannot annotate old journal entries.
    journal
        .seek_tail()
        .expect("Unable to seek to end of journal!");

    // Setup initial regex conf.
    let mut regex_confs: Vec<RegexConf> = Vec::new();

    // Read config to add more RegexConf.
    for regex_conf_str in BUILD_IN_REGEX_CONFS {
        regex_confs.push(regex_conf_str.to_regex_conf());
    }

    loop {
        let mut fds = FdSet::new();
        fds.insert(journal.as_raw_fd());
        nix::sys::select::select(None, Some(&mut fds), None, None, None)
            .unwrap();
        if !fds.contains(journal.as_raw_fd()) {
            continue;
        }

        for entry in &mut journal {
            match entry {
                Ok(entry) => {
                    process_journal_entry(&entry, sender, &regex_confs)
                }
                Err(e) => {
                    println!("Error retrieving the journal entry: {:?}", e)
                }
            }
        }
    }
}
