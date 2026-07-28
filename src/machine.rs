//! What machine is this, and how much of it should the search take?
//!
//! Two questions that used to be answered by constants. The worker count was a
//! fixed four — a number measured on an eight-core M1, where it happens to be
//! exactly right, and wrong almost everywhere else: it is the whole machine on
//! a small laptop and a quarter of a desktop. The interface said "Mac" in its
//! texts regardless of what it was running on.
//!
//! Both are now read from the hardware at startup.

/// The cores this machine actually has.
///
/// `efficiency` is zero when there is no split to detect — either the hardware
/// has none, or the platform does not tell us. Callers must treat those two
/// cases the same, because from here they are indistinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Machine {
    /// Physical cores, ignoring hyperthreads.
    pub physical: usize,
    /// Fast cores. Equal to `physical` when there is no split.
    pub performance: usize,
    /// Slow, power-saving cores. Zero when there is no split.
    pub efficiency: usize,
}

impl Machine {
    /// Reads the hardware. Cheap enough to call at startup, not in a loop.
    pub fn detect() -> Machine {
        let physical = crate::config::physical_cores();
        let (performance, efficiency) = match core_split() {
            // Trust the split only if it adds up to what we counted; a mismatch
            // means one of the two sources is describing a different machine.
            Some((p, e)) if p > 0 && p + e == physical => (p, e),
            _ => (physical, 0),
        };
        Machine {
            physical,
            performance,
            efficiency,
        }
    }

    /// Workers to start when the configuration does not name a number.
    ///
    /// The rule is "the fast half, and leave the rest alone", for two measured
    /// reasons. Throughput scales badly across the second half: on an M1, four
    /// cores reach 72 % of what eight reach, so the last four buy 38 % for
    /// double the active silicon, double the heat and double the fan. And a
    /// machine that has been handed all of its cores stops feeling like it
    /// belongs to the person using it.
    ///
    /// Where the hardware splits fast and slow cores, the fast ones *are* that
    /// half — and the slow ones actively hurt: at background priority six
    /// cores measured faster than eight, because the work was confined to four
    /// efficiency cores that then got in each other's way.
    pub fn recommended_threads(&self) -> usize {
        if self.efficiency > 0 {
            self.performance
        } else {
            (self.physical / 2).max(1)
        }
    }

    /// Workers for the quiet preset: as little of the machine as still makes
    /// progress. The efficiency cores where they exist, a quarter otherwise.
    pub fn economical_threads(&self) -> usize {
        if self.efficiency > 0 {
            self.efficiency
        } else {
            (self.physical / 4).max(1)
        }
    }

    /// Everything the machine has. The upper end of the slider.
    pub fn max_threads(&self) -> usize {
        self.physical.max(1)
    }

    /// One line for the interface: what was found and what follows from it.
    pub fn describe(&self) -> String {
        if self.efficiency > 0 {
            format!(
                "{} Kerne erkannt — {} schnelle, {} sparsame",
                self.physical, self.performance, self.efficiency
            )
        } else {
            format!("{} Kerne erkannt", self.physical)
        }
    }
}

/// How to call the machine in a German sentence.
///
/// All three words are masculine, so „dieser {}", „deinen {}" and „der {}" work
/// with any of them and the callers stay simple.
pub const fn noun() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mac"
    } else if cfg!(target_os = "windows") {
        "PC"
    } else {
        "Rechner"
    }
}

/// „dieser Mac" / „dieser PC" / „dieser Rechner".
pub fn this_machine() -> String {
    format!("dieser {}", noun())
}

/// Fast and slow core counts, where the platform will say.
///
/// Apple silicon exposes one `perflevel` per core type, fastest first. Intel
/// Macs have no such keys, and neither Windows nor Linux offers anything as
/// direct — a Linux answer would mean reading `cpu_capacity` per core and
/// clustering the values, which is a lot of guessing for a preset. Those
/// platforms report no split and get the plain half.
#[cfg(target_os = "macos")]
fn core_split() -> Option<(usize, usize)> {
    let p = sysctl_usize("hw.perflevel0.physicalcpu")?;
    // A machine with a single performance level has no efficiency cores, which
    // is a missing key rather than a zero.
    let e = sysctl_usize("hw.perflevel1.physicalcpu").unwrap_or(0);
    Some((p, e))
}

#[cfg(not(target_os = "macos"))]
fn core_split() -> Option<(usize, usize)> {
    None
}

#[cfg(target_os = "macos")]
fn sysctl_usize(name: &str) -> Option<usize> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut value: i32 = 0;
    let mut len = std::mem::size_of::<i32>() as libc::size_t;
    let rc = unsafe {
        libc::sysctlbyname(
            c.as_ptr(),
            &mut value as *mut i32 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 && value > 0 {
        Some(value as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_is_internally_consistent() {
        let m = Machine::detect();
        assert!(m.physical >= 1, "no cores found");
        assert!(m.performance >= 1);
        assert_eq!(
            m.performance + m.efficiency,
            m.physical,
            "the split has to account for every core: {m:?}"
        );
    }

    /// The whole point of the recommendation: enough to be worth running, never
    /// the whole machine unless the machine is a single core.
    #[test]
    fn recommendation_leaves_something_over() {
        let m = Machine::detect();
        let r = m.recommended_threads();
        assert!(r >= 1, "must start at least one worker");
        assert!(r <= m.physical, "cannot use more cores than exist");
        if m.physical > 1 {
            assert!(
                r < m.physical,
                "{r} of {} cores leaves nothing for the user",
                m.physical
            );
        }
    }

    /// Checked across plausible shapes, since the test machine is only ever one
    /// of them.
    #[test]
    fn recommendation_holds_for_machines_we_do_not_have() {
        let cases = [
            // physical, performance, efficiency, expected
            (1, 1, 0, 1),   // single core: it gets used
            (2, 2, 0, 1),   // dual core: half
            (4, 4, 0, 2),   // small desktop
            (16, 16, 0, 8), // big desktop: half, not sixteen
            (8, 4, 4, 4),   // M1: the performance cores
            (12, 6, 6, 6),  // M3 Pro shape
            (10, 8, 2, 8),  // M1 Pro shape: mostly fast cores
        ];
        for (physical, performance, efficiency, want) in cases {
            let m = Machine {
                physical,
                performance,
                efficiency,
            };
            assert_eq!(
                m.recommended_threads(),
                want,
                "wrong recommendation for {m:?}"
            );
            assert!(m.economical_threads() >= 1);
            assert!(m.economical_threads() <= m.recommended_threads());
        }
    }

    #[test]
    fn the_machine_is_named_after_its_platform() {
        let expected = if cfg!(target_os = "macos") {
            "Mac"
        } else if cfg!(target_os = "windows") {
            "PC"
        } else {
            "Rechner"
        };
        assert_eq!(noun(), expected);
        assert_eq!(this_machine(), format!("dieser {expected}"));
    }

    #[test]
    fn description_mentions_every_core() {
        let m = Machine {
            physical: 8,
            performance: 4,
            efficiency: 4,
        };
        let d = m.describe();
        assert!(d.contains('8') && d.contains('4'), "{d}");

        let plain = Machine {
            physical: 6,
            performance: 6,
            efficiency: 0,
        };
        assert!(plain.describe().contains('6'));
        assert!(
            !plain.describe().contains("sparsame"),
            "a machine without a split must not claim one"
        );
    }
}
