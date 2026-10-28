-- Sign-in by any OIDC issuer: an identity is (issuer, subject), and a user holds at most one per
-- issuer. Entra rows keep their binding: issuer is the tenant's v2.0 issuer URL, subject the oid.
ALTER TABLE identities RENAME COLUMN tid TO issuer;
ALTER TABLE identities RENAME COLUMN oid TO subject;
UPDATE identities SET issuer = 'https://login.microsoftonline.com/' || issuer || '/v2.0';
ALTER TABLE identities DROP CONSTRAINT identities_user_id_key;
ALTER TABLE identities ADD CONSTRAINT identities_one_per_issuer UNIQUE (user_id, issuer);
