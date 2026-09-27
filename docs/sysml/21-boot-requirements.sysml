/*
 * Ferrix — SysML v2 model, part 21: the low-level requirements of the boot.
 *
 * What each unit of the kernel's bring-up does, as `ItemLowLevel`
 * requirements (part 13 defines the format, part 14 the lessons they
 * follow): the crate root's own functions -- the entry, the stages kmain
 * runs in order and the end of boot -- and init.rs, power.rs, random.rs and
 * checks.rs. The stage checks main.rs used to hold moved to stages_check.rs
 * (21a), where a tag may go on them; what here also brings something up
 * stays, with a requirement for what it brings up. devmgr.rs is 21c.
 *
 * One requirement per behaviour one check can pass or fail. Where no check
 * proves one yet it waits in the baseline as check-writing work.
 *
 * Ids are `L.boot.<n>`; the package gives the file.
 */
package FerrixBootRequirements {
    private import FerrixItemRequirements::*;

    package Entry {
        doc /* kernel/src/main.rs: the entry, kmain's stages, and the end of
             * boot. */

        requirement <'L.boot.1'> theMarkerAfterTheChecks : ItemLowLevel {
            attribute :>> statement = "When every self-check of stages 1 to 12 has
                run and passed, the boot shall print FERRIX-BOOT-OK stages 1-12 as
                its last line before init starts.";
            attribute :>> criterion = "test-boot reaches FERRIX-BOOT-OK stages 1-12,
                with no panic before it, on the architecture it boots.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = ("_start", "kmain", "say_booted");
        }
        requirement <'L.boot.2'> anUncheckedBootSaysSo : ItemLowLevel {
            attribute :>> statement = "A boot with ferrix.checks=skip shall print
                FERRIX-BOOT-UNCHECKED stages 1-12 where the success marker would
                be.";
            attribute :>> criterion = "test-init's boot with ferrix.checks=skip
                reaches FERRIX-BOOT-UNCHECKED.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = ("say_booted", "checks::run");
        }
        requirement <'L.boot.3'> aFailedHandOffStops : ItemLowLevel {
            attribute :>> statement = "A stage 1 check of the loader's hand-off, its
                page tables or the kernel's layout that fails shall stop the boot
                with the stage 1 explanation before anything uses what it
                checked.";
            attribute :>> criterion = "A boot handed a hand-off, a table or a layout
                that fails one of stage 1's checks stops at stage 1 with that
                explanation.";
            attribute :>> parent = "H.BOOT.1";
            attribute :>> unit = ("self_check", "report", "report_layout",
                "check_early_mapper", "framebuffer_bytes");
        }
        requirement <'L.boot.4'> memoryFromTheHandOff : ItemLowLevel {
            attribute :>> statement = "bring_up_memory shall start the frame
                allocator and the heap from the memory map the hand-off gave, say
                what they manage, and stop the boot if either cannot start.";
            attribute :>> criterion = "The frames line gives the memory managed and
                free; a memory map with nothing usable stops the boot.";
            attribute :>> parent = "H.BOOT.1";
            attribute :>> unit = "bring_up_memory";
        }
        requirement <'L.boot.5'> earlyMemoryFinished : ItemLowLevel {
            attribute :>> statement = "finish_memory shall take down the loader's
                identity map, then sweep every mapping for W^X, then give early
                boot's memory back, in that order, and stop the boot if any step
                fails.";
            attribute :>> criterion = "After the identity map is gone the sweep finds
                no mapping writable and executable, and the memory early boot held
                is back in the allocator.";
            attribute :>> parent = "H.MEM.4";
            attribute :>> unit = "finish_memory";
        }
        requirement <'L.boot.6'> theClockStarted : ItemLowLevel {
            attribute :>> statement = "The boot shall start the realtime clock from
                firmware's time, or at the epoch where firmware has none, after
                stage 3's timer check when the checks run, and say which.";
            attribute :>> criterion = "The clock line names firmware's clock and
                its seconds, or says CLOCK_REALTIME starts at the epoch.";
            attribute :>> parent = "H.BOOT.10";
            attribute :>> unit = ("check_timer_and_start_clocks", "report_stage3",
                "report_clock_and_random", "report_clocks", "report_clocks_and_power");
        }
        requirement <'L.boot.7'> processorsBroughtUp : ItemLowLevel {
            attribute :>> statement = "bring_up_processors shall start every
                processor firmware describes and stop the boot if one does not come
                online, then say how many are online and what each is exposed
                to.";
            attribute :>> criterion = "The cpus line gives as many online as
                described; a processor that never comes online stops the boot.";
            attribute :>> parent = "H.BOOT.5";
            attribute :>> unit = ("bring_up_processors", "report_processors");
        }
        requirement <'L.boot.8'> schedulerStarted : ItemLowLevel {
            attribute :>> statement = "start_scheduler shall start the scheduler on
                every online processor and stop the boot if it cannot.";
            attribute :>> criterion = "Every online processor runs a task after
                start_scheduler; a scheduler that cannot start stops the boot.";
            attribute :>> parent = "H.SCHED.1";
            attribute :>> unit = "start_scheduler";
        }
        requirement <'L.boot.9'> consoleInputInstalled : ItemLowLevel {
            attribute :>> statement = "start_console_input shall install the port's
                receive interrupt before interrupts are first enabled, so that a
                byte already waiting reaches the input ring.";
            attribute :>> criterion = "A byte the port held before interrupts were
                enabled is read from the input ring.";
            attribute :>> parent = "H.BOOT.8";
            attribute :>> unit = "start_console_input";
        }
        requirement <'L.boot.10'> theLoadRegistered : ItemLowLevel {
            attribute :>> statement = "register_load shall register every interface
                the item reaches the load ring through, and stop the boot with the
                load registration explanation if one is missing or a list is
                full.";
            attribute :>> criterion = "The hooks lines count what was registered; a
                boot with one registration missing stops with the load registration
                explanation.";
            attribute :>> parent = "H.BOOT.11";
            attribute :>> unit = "register_load";
        }
        requirement <'L.boot.11'> theAuditStarted : ItemLowLevel {
            attribute :>> statement = "start_audit shall start the audit record with
                the boot's audit id and record whether the self-checks run, who
                starts devmgr, the mitigations and the KASLR state, as bring-up
                read them.";
            attribute :>> criterion = "The kernel's boot records begin with the
                start-up record of the id reads answer, and hold the 4
                configuration records with bring-up's values.";
            attribute :>> parent = "H.AUD.7";
            attribute :>> unit = "start_audit";
        }
        requirement <'L.boot.12'> theRootBuilt : ItemLowLevel {
            attribute :>> statement = "check_filesystems shall build the root from
                the initramfs, stop the boot if it cannot, and say what was
                unpacked and whether its check ran.";
            attribute :>> criterion = "The initrd line gives the directories, files
                and links unpacked and verified true, or says the root is an empty
                tmpfs.";
            attribute :>> parent = "H.BOOT.12";
            attribute :>> unit = ("check_filesystems", "report_initrd");
        }
        requirement <'L.boot.13'> pciEnumerated : ItemLowLevel {
            attribute :>> statement = "check_pci shall enumerate every PCI function
                the firmware's hosts describe, and stop the boot if enumeration
                fails.";
            attribute :>> criterion = "The pci line counts the functions found under
                each host, or says no host is described.";
            attribute :>> parent = "H.DMA.1";
            attribute :>> unit = "check_pci";
        }
        requirement <'L.boot.14'> devicesPublished : ItemLowLevel {
            attribute :>> statement = "check_devices shall publish a device node for
                every function found, each holding exactly its apertures and
                vectors, and stop the boot if publishing fails.";
            attribute :>> criterion = "The devices line counts the nodes, apertures
                and vectors published; a node that would hand out more stops the
                boot.";
            attribute :>> parent = "H.DMA.5";
            attribute :>> unit = "check_devices";
        }
        requirement <'L.boot.15'> theKernelStartsDevmgr : ItemLowLevel {
            attribute :>> statement = "Without ferrix.devmgr=init, check_block_ring
                shall have the kernel start devmgr with every device, and stop the
                boot if devmgr cannot start.";
            attribute :>> criterion = "The devmgr line counts the devices, drivers
                started and failed; an image without devmgr says so.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = ("check_block_ring", "check_devmgr");
        }
        requirement <'L.boot.16'> theDriverServesItsDisk : ItemLowLevel {
            attribute :>> statement = "check_driver shall have the block driver
                serve the test disk through the block ring with the IOMMU on, and
                leave it running for what mounts it next.";
            attribute :>> criterion = "Sectors read through the registry come back
                as xtask wrote them; a machine without the disk says so.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = "check_driver";
        }
        requirement <'L.boot.17'> aVolumeWrittenAndReadBack : ItemLowLevel {
            attribute :>> statement = "check_btrfs_write shall mount a blank btrfs
                volume writable, write it, remount it, and read back what it
                wrote.";
            attribute :>> criterion = "Every byte written is read back after the
                remount; a machine without the third disk says so.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = "check_btrfs_write";
        }
        requirement <'L.boot.18'> theNetCoreStarted : ItemLowLevel {
            attribute :>> statement = "check_net shall start the net core's task,
                and stop the boot if it cannot.";
            attribute :>> criterion = "The net core's task runs; one that cannot
                start stops the boot with the net core explanation.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = "check_net";
        }
        requirement <'L.boot.19'> devmgrLeftToPid1 : ItemLowLevel {
            attribute :>> statement = "Under ferrix.devmgr=init, check_block_ring
                shall start no driver itself, make the drivers' job for pid 1's
                devmgr, and switch / once devmgr has reported.";
            attribute :>> criterion = "The devmgr line says it is left to pid 1, and
                / switches after devmgr reports.";
            attribute :>> parent = "H.BOOT.13";
            attribute :>> unit = "check_block_ring";
        }
    }

    package Init {
        doc /* kernel/src/init.rs: starting pid 1, and what it is handed. */

        requirement <'L.boot.20'> theFirstLauncherStands : ItemLowLevel {
            attribute :>> statement = "The first launcher registered shall be the one
                init is started with; a later registration shall change
                nothing.";
            attribute :>> criterion = "A second registration leaves the first
                launcher in place.";
            attribute :>> parent = "H.BOOT.11";
            attribute :>> unit = ("init::register_launcher", "init::has_launcher");
        }
        requirement <'L.boot.21'> theHelloFirst : ItemLowLevel {
            attribute :>> statement = "A bootstrap channel shall hold the kernel's
                hello, and only it, when the program is given its end.";
            attribute :>> criterion = "Pid 1's bootstrap channel holds one message,
                the hello with version 1 and no handles, and its peer is open.";
            attribute :>> parent = "H.BOOT.13";
            attribute :>> unit = "init::bootstrap_channel";
        }
        requirement <'L.boot.22'> pid1GivenItsHandles : ItemLowLevel {
            attribute :>> statement = "The first program shall be given devmgr's
                starter, under ferrix.devmgr=init, and the audit record's handle
                on its bootstrap channel.";
            attribute :>> criterion = "The audit record read back in test-init holds
                STARTER_GIVEN and READER_GIVEN naming pid 1.";
            attribute :>> parent = "H.BOOT.13";
            attribute :>> unit = ("init::next_bootstrap", "init::give_audit");
        }
        requirement <'L.boot.23'> givenOnce : ItemLowLevel {
            attribute :>> statement = "The starter and the audit record's handle
                shall be given once: a later program of a command list gets
                neither.";
            attribute :>> criterion = "A second program started by the kernel finds
                neither on its channel.";
            attribute :>> parent = "H.BOOT.13";
            attribute :>> unit = ("init::next_bootstrap", "init::give_audit");
        }
        requirement <'L.boot.24'> theSwitchRecorded : ItemLowLevel {
            attribute :>> statement = "notify_root shall record where / is: the root
                volume with pid 1 moved onto it, or the tmpfs.";
            attribute :>> criterion = "The audit record read back in test-init holds
                ROOT_SWITCHED with pid 1 moved.";
            attribute :>> parent = "H.BOOT.13";
            attribute :>> unit = "init::notify_root";
        }
        requirement <'L.boot.25'> pid1ToldWhereRootIs : ItemLowLevel {
            attribute :>> statement = "notify_root shall tell pid 1 on its bootstrap
                channel where / is, once.";
            attribute :>> criterion = "Pid 1 reads one root message after the hello,
                with the value the switch reached.";
            attribute :>> parent = "H.BOOT.13";
            attribute :>> unit = "init::notify_root";
        }
        requirement <'L.boot.26'> pid1AsNamed : ItemLowLevel {
            attribute :>> statement = "ferrix.init shall name the file pid 1 is
                started from, a #! script running under its interpreter.";
            attribute :>> criterion = "A boot with ferrix.init naming a #!/bin/sh
                script runs the script and reports the status it exited with.";
            attribute :>> parent = "H.BOOT.15";
            attribute :>> unit = ("init::read_option", "init::run", "init::run_file",
                "init::start", "init::Failure::fmt");
        }
        requirement <'L.boot.27'> aMissingInitIsSaid : ItemLowLevel {
            attribute :>> statement = "A file ferrix.init names that cannot be
                started shall be said on one line, and the built-in program shall
                run instead.";
            attribute :>> criterion = "A boot naming a file the image lacks says it
                could not be started and runs the built-in program.";
            attribute :>> parent = "H.BOOT.15";
            attribute :>> unit = ("init::run", "init::run_built_in",
                "init::Refusal::fmt");
        }
        requirement <'L.boot.28'> theDefaultOrder : ItemLowLevel {
            attribute :>> statement = "With nothing named, the kernel shall run the
                built-in program or commands, then /sbin/init if the image has one,
                then nothing.";
            attribute :>> criterion = "A boot with no ferrix.init runs the built-in
                program; an image with /sbin/init and nothing built in runs it.";
            attribute :>> parent = "H.BOOT.15";
            attribute :>> unit = ("init::run_default", "init::run_built_in",
                "init::run_commands", "init::parse", "init::Argv::fmt",
                "init::no_memory");
        }
    }

    package Power {
        doc /* kernel/src/power.rs: how boot ends. */

        requirement <'L.boot.29'> committedBeforeItStops : ItemLowLevel {
            attribute :>> statement = "Every flush a filesystem registered shall be
                committed, in the order registered, before the machine stops,
                whoever stops it.";
            attribute :>> criterion = "A file written to /data and powered off with
                poweroff -f -n, which does not sync, is read back on the next
                boot.";
            attribute :>> parent = "H.BOOT.6";
            attribute :>> unit = ("power::register_flush", "power::flushes",
                "power::sync_disks");
        }
        requirement <'L.boot.30'> resetWhenAsked : ItemLowLevel {
            attribute :>> statement = "With ferrix.onexit=reset the kernel shall say
                so at boot and reset the machine when init exits.";
            attribute :>> criterion = "The power line arms the reset, and the
                machine resets after init exits.";
            attribute :>> parent = "H.BOOT.7";
            attribute :>> unit = ("power::init", "power::finish");
        }
        requirement <'L.boot.31'> anUnknownOnexitPowersOff : ItemLowLevel {
            attribute :>> statement = "A ferrix.onexit value the kernel does not know
                shall be said and ignored, and the machine powered off when init
                exits.";
            attribute :>> criterion = "A boot with ferrix.onexit=halt says it is not
                understood and powers off.";
            attribute :>> parent = "H.BOOT.6";
            attribute :>> unit = ("power::init", "power::finish");
        }
        requirement <'L.boot.32'> theBootModeWord : ItemLowLevel {
            attribute :>> statement = "A word reboot(2) passes shall go to the boot
                mode a board registered, and be said as changing nothing where none
                is.";
            attribute :>> criterion = "On the STM32MP157D-DK1, reboot with a word
                sets the boot mode U-Boot reads; elsewhere the word is said to
                change nothing.";
            attribute :>> parent = "H.BOOT.7";
            attribute :>> unit = ("power::register_boot_mode", "power::has_boot_mode",
                "power::request_boot_mode");
        }
    }

    package Random {
        doc /* kernel/src/random.rs: the kernel's generator. */

        requirement <'L.boot.33'> seededBeforePrograms : ItemLowLevel {
            attribute :>> statement = "random::init shall seed the generator from
                firmware's seed, the TRNG, the processor and timer jitter, and say
                whether it reached full seeding and from what.";
            attribute :>> criterion = "The random line says seeded with its bits and
                their sources, or NOT SEEDED with how far it got.";
            attribute :>> parent = "H.BOOT.14";
            attribute :>> unit = "random::init";
        }
        requirement <'L.boot.34'> twoReadsDiffer : ItemLowLevel {
            attribute :>> statement = "Two reads of the generator shall differ, and a
                read shall not be all zeros.";
            attribute :>> criterion = "Two reads of 64 bytes differ, and the first is
                not all zeros.";
            attribute :>> parent = "H.BOOT.14";
            attribute :>> unit = "random::fill";
        }
    }

    package Checks {
        doc /* kernel/src/checks.rs: ferrix.checks, the self-checks' switch
             * (F-51: product code, counted as verification until 21b). */

        requirement <'L.boot.35'> aMisspeltValueRunsEveryCheck : ItemLowLevel {
            attribute :>> statement = "A ferrix.checks value the kernel does not know
                shall be said, and every self-check shall run.";
            attribute :>> criterion = "A boot with ferrix.checks=skipp says the value
                is not understood before it reaches FERRIX-BOOT-OK.";
            attribute :>> parent = "H.BOOT.2";
            attribute :>> unit = "checks::init";
        }
    }
}
