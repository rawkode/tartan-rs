## ADDED Requirements

### Requirement: Tokens have scopes, an optional list of repositories and an expiry
Scopes SHALL be any of `read`, `write` and `admin`. `repos`, when given, SHALL list repository paths (`acme/platform/api`) or group prefixes (`acme/platform/*`, matching every repository at any depth below that group), and the token SHALL be refused with 403 on every route not addressed to a listed repository or, for reads, a group above one, including `POST /tree`, `PUT /placements` and `/subscribers/**`, except `GET /-/api/me` and `GET /-/api/people`. The expiry SHALL be 1 to 365 days. Unknown scopes or malformed entries SHALL be refused with 400.

#### Scenario: Repository-limited token
- **WHEN** a token limited to `acme/web` reads `acme/api`'s extensions
- **THEN** it is refused with 403

#### Scenario: Group-limited token
- **WHEN** a token limited to `acme/platform/*` reads the extensions of `acme/platform/tools/lint`
- **THEN** the answer is returned, and the same token reading `acme/web`'s extensions is refused with 403

#### Scenario: Repo-limited admin token creates a group
- **WHEN** an owner's `admin` token limited to `acme/web` calls `POST /tree`, `PUT /placements` or reads a subscriber's event stream
- **THEN** each is refused with 403

#### Scenario: Repo-limited token asks who it is
- **WHEN** a token limited to `acme/web` calls `GET /-/api/me`
- **THEN** it answers the token's principal

#### Scenario: Too long
- **WHEN** a token is requested with `expires_in_days: 400`
- **THEN** it is refused with 400

## REMOVED Requirements

### Requirement: Tokens have scopes, an optional repository list and an expiry
**Reason**: Orgs are removed, so the requirement is restated for the tree.
**Migration**: See "Tokens have scopes, an optional list of repositories and an expiry".
