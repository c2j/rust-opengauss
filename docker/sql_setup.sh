#!/bin/bash
set -e

cat > "$PGDATA/server.key" <<-EOKEY
-----BEGIN PRIVATE KEY-----
MIIEvwIBADANBgkqhkiG9w0BAQEFAASCBKkwggSlAgEAAoIBAQDByjd3/KuaHc6u
RVLVBPUCGHmBVrO3PrE+uqZ4F5mso0xlO9+agTLaMwlVUZCP40/YPJJMD3MpGmiq
njTGNzk00xfpUSNwMHKn0mhFcJZ0I1kucfMlv8MCXQE42ZW95sKeksqnMwYeQwef
Ep+EtglKiJLKemHLuSS03hFd+mAw/xeR53ngWybcNq7Um0J6tvaqF3gmpQisOHv9
W39AEEG9dyVe9zkD1lirEjBMsySUCYXQZsaAVkZvcEuDpENKCkf7hvr6iR+E5+ic
flP/n4btvfPN6uksnbfXXBqGayeUXc8DifQOMjB7KM+6s4KXwRy5mPiRhoSqbUGI
I1/XXKB1AgMBAAECggEBAKHUBE4mqgahLZ9KNMm+wftmBNGFXb6Ak/MWWz2eN657
D6jaPvf/PEAKrpiY7Ge8I0koC+OIY1rHuu///YIpS5RZU3Z7U2S0kIqkon6abom9
mxO2BJ5ZbLfkgsi+qtVShuN1IdZOPaW3w/2Kx7tDaK7dLff4CwLdMVH4v/gCxaEj
bTpvJLLX0aicE3+LS8XTPQ/2vpFZmUz3MfX0K+StBA051cZtZSvWoCDtzc+LxO3W
8dZCXdfNdV2nJKy/pco+Q92nA+xnNu9RiaARjgBvRKFg0GjWYsjgMcPj4feK2J6c
JIeueUrY36CzuC4UQWZQRwTDO12HMI5e7bVfaAImZWECgYEA/555yftpHyfOo/LH
TtjCEoWsn728fWVXV/F6MbfUh3KUZAdhu9dqrGiW+uT+KtIlDJVCBfqyHv0uua0T
GIi46Vkq+pcMPofcxdnIEg5uTP7UWbCxq6g2+YDgAMy0s4BZXSxpt/T81Y4j4j/s
RZl+VKLQoEl2bKLsxvapxknOVNcCgYEAwhQm2jVq9IFpXtbBv9YNRsUpeht7j3mL
tnPO4gQ0zSR2vjKPL3GFLP3Sl/K4XCsQ5PFnV86F0YyxJkVQdBehT4j5Y4CDHNUJ
rAPPc4QotTvc6GReC8md9xZH1TDXC/cH6QJ7ucDiOwkR1Xvq5z+qafzp8t2pax3F
IMYtiMfnP5MCgYEA0A/OUfmxtwpPyGL0l9kXHrxvphZqNicm0Q5cx9s5woYhAsp/
YsYUrgDz44RA3dnvDi7vbq6ADXlHbxrRUEb5O/a4ZQBNlxg/O0vo7cmRPlqtvdN0
yqRBGxUrP3tgGjt+gbiE1Jc0tR7dVmtxhbVKftmHtvAU1JhI4iokRqIMsEECgYB7
b3muex8FV3F/AjPEIQ3cnvVcVjJl9DYp2soP8gDrIG/tVBbBZAABt4XDYnpjFHjw
Q6EotY9i0Yqx/o/G5miQP1vuLwQ0yEIYh2vf2oRRkDtWCs/Ny3OOfTs+mouLbpg3
WH78i3LXfVM8Zk3muhVWx6a78sMX/50q1SCMyCeJxQKBgQDmfI3Qs32m0duCD+04
HbjYjc0hgSgNWV9hccCjwsCb2j9aEcbjL6hh6HfmZ7PVDmlGKN8VEcFQqWGCzci+
74pew/k7X8zMmQQVkx8zaoH9mjJG/AodE/2xzd7MZDRZE1qLeZoVe9lApKUY5yLt
pOml5P6vzAr+jBSWq0UUG8HeOA==
-----END PRIVATE KEY-----
EOKEY
chmod 0600 "$PGDATA/server.key"

cat > "$PGDATA/server.crt" <<-EOCERT
-----BEGIN CERTIFICATE-----
MIIDSDCCAjCgAwIBAgIJAOKhpezdNqHqMA0GCSqGSIb3DQEBCwUAMFkxCzAJBgNV
BAYTAkFVMRMwEQYDVQQIDApTb21lLVN0YXRlMSEwHwYDVQQKDBhJbnRlcm5ldCBX
aWRnaXRzIFB0eSBMdGQxEjAQBgNVBAMMCWxvY2FsaG9zdDAeFw0yNjA2MjcwNzI2
MThaFw00NjA2MjIwNzI2MThaMFkxCzAJBgNVBAYTAkFVMRMwEQYDVQQIDApTb21l
LVN0YXRlMSEwHwYDVQQKDBhJbnRlcm5ldCBXaWRnaXRzIFB0eSBMdGQxEjAQBgNV
BAMMCWxvY2FsaG9zdDCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAMHK
N3f8q5odzq5FUtUE9QIYeYFWs7c+sT66pngXmayjTGU735qBMtozCVVRkI/jT9g8
kkwPcykaaKqeNMY3OTTTF+lRI3AwcqfSaEVwlnQjWS5x8yW/wwJdATjZlb3mwp6S
yqczBh5DB58Sn4S2CUqIksp6Ycu5JLTeEV36YDD/F5HneeBbJtw2rtSbQnq29qoX
eCalCKw4e/1bf0AQQb13JV73OQPWWKsSMEyzJJQJhdBmxoBWRm9wS4OkQ0oKR/uG
+vqJH4Tn6Jx+U/+fhu29883q6Sydt9dcGoZrJ5RdzwOJ9A4yMHsoz7qzgpfBHLmY
+JGGhKptQYgjX9dcoHUCAwEAAaMTMBEwDwYDVR0TAQH/BAUwAwEB/zANBgkqhkiG
9w0BAQsFAAOCAQEAC/4RZ8w+N7CTMwi1uBcd/tYZeDpr581tnFz8CipS6g4H8qUH
M6dEW/zlF+nZ5qJWl9z97MX/xIkgntVPfERl+XF7W/DKgox80T8YhEAbcbgi0N25
ghh2zpnMQjVJK6fX1OmHgRndQq/ZB0DcKuft6b2MqA7jKYFqMd0lY2SgP2Fh2Uky
JpqgzFVHZeJ3alYuxi1OyKNYwV+4g0FPFBCMieklA9KaB40ExxKPtehIbNX5q1l2
fJis+2vy2DvL1Zx2UY3qTbnBlbgZZEPhft6cte/KugdREC8f0AoH8RBG2V1Oh2NZ
YJdGW9qGFkMrLGSSi/IDBMO9WBIHzBggrtGWkA==
-----END CERTIFICATE-----
EOCERT

cat >> "$PGDATA/postgresql.conf" <<-EOCONF
port = 5433
ssl = on
ssl_cert_file = 'server.crt'
ssl_key_file = 'server.key'
EOCONF

cat > "$PGDATA/pg_hba.conf" <<-EOCONF
# TYPE  DATABASE        USER            ADDRESS                 METHOD
host    all             pass_user       0.0.0.0/0            password
host    all             md5_user        0.0.0.0/0            md5
host    all             scram_user      0.0.0.0/0            scram-sha-256
host    all             pass_user       ::0/0                password
host    all             md5_user        ::0/0                md5
host    all             scram_user      ::0/0                scram-sha-256

hostssl all             ssl_user        0.0.0.0/0            trust
hostssl all             ssl_user        ::0/0                trust
host    all             ssl_user        0.0.0.0/0            reject
host    all             ssl_user        ::0/0                reject

# IPv4 local connections:
host    all             postgres        0.0.0.0/0            trust
# IPv6 local connections:
host    all             postgres        ::0/0                trust
# Unix socket connections:
local   all             postgres                             trust
EOCONF

psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" <<-EOSQL
    SET password_encryption TO 'md5';
    CREATE ROLE pass_user PASSWORD 'password' LOGIN;
    CREATE ROLE md5_user PASSWORD 'password' LOGIN;
    SET password_encryption TO 'scram-sha-256';
    CREATE ROLE scram_user PASSWORD 'password' LOGIN;
    CREATE ROLE ssl_user LOGIN;
    CREATE EXTENSION hstore;
    CREATE EXTENSION citext;
    CREATE EXTENSION ltree;
EOSQL
