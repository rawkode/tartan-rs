# Spec Delta

## ADDED Requirements

### Requirement: Configuration waits for the signing check
When the signing rule covers `refs/heads/main`, the forge SHALL apply a configuration from `main` only after the update that set `main` to it has been checked, judged on that update's own result, and SHALL refuse it with `config.refused` naming the first object not verified by an allowed signer. Older `main` updates SHALL not delay it. Waiting SHALL NOT use the retry budget; completing the check SHALL apply or refuse it without another push.

#### Scenario: Unsigned configuration change
- **WHEN** `main` requires signatures and an unsigned commit changing Issues' settings is pushed to it
- **THEN** `config.refused` is recorded naming the commit and Issues keeps its settings

#### Scenario: Unsigned configuration via another branch
- **WHEN** `main` requires signatures, an unsigned commit changing Issues' settings is pushed to an unprotected branch, and `main` is then fast-forwarded to it
- **THEN** `config.refused` is recorded naming the commit and Issues keeps its settings

#### Scenario: Rewind past the rule
- **WHEN** `main` requires signatures and is rewound to an unsigned commit from before the rule existed
- **THEN** `config.refused` is recorded naming that commit, its configuration is not applied, and the signing rule stays in force

#### Scenario: Signed configuration change
- **WHEN** the same change is pushed in a commit verified as an allowed signer
- **THEN** it is applied

#### Scenario: No rule on main
- **WHEN** no signing rule covers `refs/heads/main`
- **THEN** configuration is applied without waiting for signature checks

#### Scenario: PDS down after a signed configuration push
- **WHEN** `main` requires signatures and the signer's PDS is down for 10 minutes after a signed configuration change is pushed
- **THEN** `config.applied` is recorded once the check completes, without another push or any other request reaching the forge

#### Scenario: Earlier update still retrying
- **WHEN** `main` requires signatures, a signed configuration push W waits because its signer's PDS is down, and a second signed configuration push X by another signer whose PDS is up follows
- **THEN** X's configuration is applied once X's check completes, judged on X's update alone, without waiting for W, and W's later result does not change what is shown for `main`

#### Scenario: Waiting is visible
- **WHEN** a `main` configuration waits for its signature check
- **THEN** the repository's configuration status reports it as waiting on the signature check of the named commit
