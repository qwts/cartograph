Offline TLS test identity only. These fixtures are never used by application
clients. The test explicitly trusts ca.der for successful HTTPS and omits that
trust for the rejection case; a mismatched hostname is also rejected.

server-key.der is the deliberately public PKCS#8 key of this test server.
The P-256 test CA and server certificate were created using OpenSSL with a
20-year test lifetime. server.cnf records the server's extensions. No production
credentials or trust changes are involved.
