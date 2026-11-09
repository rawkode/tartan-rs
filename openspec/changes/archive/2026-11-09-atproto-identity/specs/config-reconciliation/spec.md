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

#### Scenario: Configuration whose signature was revoked
- **WHEN** `main` requires Alice's signature, a configuration commit verified as Alice on an unprotected branch has its signature record deleted and the forge learns of it, and `main` is then fast-forwarded to it
- **THEN** `config.refused` is recorded naming the commit's signature as revoked, and the previous configuration stays applied

#### Scenario: Signed configuration change
- **WHEN** the same change is pushed in a commit verified as an allowed signer
- **THEN** it is applied

#### Scenario: No rule on main
- **WHEN** no signing rule covers `refs/heads/main`
- **THEN** configuration is applied without waiting for signature checks

#### Scenario: PDS down after a signed configuration push
- **WHEN** `main` requires signatures and the signer's PDS is down, or the signature record is still in the signer's local queue, for 10 minutes after a signed configuration change is pushed
- **THEN** `config.applied` is recorded once the check completes, without another push or any other request reaching the forge

#### Scenario: Earlier update still retrying
- **WHEN** `main` requires signatures, a signed configuration push W waits because its signer's PDS is down, and a second signed configuration push X by another signer whose PDS is up follows
- **THEN** X's configuration is applied once X's check completes, judged on X's update alone, without waiting for W, and W's later result does not change what is shown for `main`

#### Scenario: Head check gives up
- **WHEN** `main` requires signatures and the check of the update that set `main` to a configuration ends `unverifiable`
- **THEN** `config.refused` is recorded with `signing: <oid> is unverifiable`, and an owner's or the signer's re-check can still apply it

#### Scenario: Waiting is visible
- **WHEN** a `main` configuration waits for its signature check
- **THEN** the repository's configuration status reports it as waiting on the signature check of the named commit

### Requirement: The signing rule changes only in a descendant
When a signing rule is in force, the forge SHALL apply a `main` configuration whose `signing` differs from that rule, including one without `signing`, only if the commit whose configuration set the rule is the new head or one of its ancestors, whatever the update's old value was. Otherwise it SHALL record `config.refused` with `signing: the signing rule can only change in a descendant of <commit>` and keep the rule and the previous configuration in force.

#### Scenario: Rewind to a signed commit from before the rule
- **WHEN** `main` requires Alice's signature and is force-pushed back to an ancestor that Alice signed before the rule existed, whose configuration has no `signing`
- **THEN** the update is compliant, `config.refused` is recorded naming the commit that set the rule, and the signing rule stays in force

#### Scenario: Unrelated signed history replaces main
- **WHEN** `main` requires Alice's signature and is force-pushed to an unrelated history of commits all signed by Alice whose configuration has no `signing`
- **THEN** `config.refused` is recorded naming the commit that set the rule, and the signing rule stays in force

#### Scenario: Main deleted and recreated at a pre-rule commit
- **WHEN** `main` requires Alice's signature, is deleted, and is then recreated at a commit Alice signed before the rule existed
- **THEN** `config.refused` is recorded naming the commit that set the rule, and the signing rule stays in force

#### Scenario: Rule changed in a signed descendant
- **WHEN** Alice pushes a signed commit on top of `main` that removes the signing rule
- **THEN** its configuration is applied and the rule is removed

#### Scenario: Revert that keeps the rule
- **WHEN** `main` is rewound to an ancestor Alice signed after the rule was set, whose configuration has the same rule but older extension settings
- **THEN** its configuration is applied and the rule stays in force

#### Scenario: Ancestry cannot be checked
- **WHEN** a configuration that changes the rule reaches `main` and the Git host cannot serve the commits needed to check ancestry until reconciliation's retries run out
- **THEN** `config.refused` is recorded and the signing rule stays in force
