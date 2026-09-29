# Authentication

How sessions and credentials are managed across the platform.

## Refreshing tokens

How should authentication tokens be refreshed? Access tokens are deliberately
short-lived. When one expires, exchange the stored refresh token at the token
endpoint; the identity provider returns a new access token together with a
rotated refresh token. Persist the rotated refresh token.

## Revocation

Revoking a refresh token invalidates every access token derived from it.
