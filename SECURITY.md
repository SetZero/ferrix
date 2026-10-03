# Security

Ferrix is a research operating system. Do not use it to protect anything
valuable yet: authentication, namespaces and seccomp are still being written
(see [the roadmap](docs/roadmap/README.md)).

Kernel memory-safety bugs, privilege escalation from ring 3, IOMMU escapes by
a ring-3 driver and btrfs images that crash the kernel are all in scope, and
we want to hear about them.

Report them privately through GitHub's
[private vulnerability reporting](https://github.com/ferrix-os/ferrix/security/advisories/new).
Please include the commit, the architecture and a way to reproduce. We will
answer within a week and credit you in the fix unless you ask us not to.
