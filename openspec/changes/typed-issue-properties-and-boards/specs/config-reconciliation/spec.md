# Spec Delta

## REMOVED Requirements

### Requirement: The org's tartan repository configures the org
**Reason**: Org-wide configuration cannot be evaluated natively by `cue eval` of one repository; every extension installs on a repository.
**Migration**: Configure each repository's extensions in its own package `tartan`; share definitions by publishing a CUE module and importing it.

### Requirement: Repositories contribute settings to org installations
**Reason**: There are no org installations to contribute to.
**Migration**: Configure the extension on the repository itself.
