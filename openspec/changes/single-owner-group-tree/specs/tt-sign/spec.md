## MODIFIED Requirements

### Requirement: Re-checking a late signature
`tt sign recheck <commit> --repo <path>` SHALL ask the signed-in forge to re-check the commit's `unverifiable` result as its signer, after publishing the commit's record from the queue if it is still there. `<path>` is the repository's tree path, such as `acme/platform/api` or `notes`.

#### Scenario: Published after the forge gave up
- **WHEN** a person whose commit's record was queued for eight days runs `tt sign recheck <commit> --repo acme/api`
- **THEN** the record is published, the forge re-checks the commit, and it ends `verified`

#### Scenario: Nested repository
- **WHEN** a person runs `tt sign recheck <commit> --repo acme/platform/api`
- **THEN** the forge is asked about the repository at that path
