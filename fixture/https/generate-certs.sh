#!/usr/bin/env sh
set -eu

cert_dir="$(CDPATH= cd -- "$(dirname -- "$0")/certs" && pwd)"
cd "$cert_dir"

openssl req -x509 -newkey rsa:2048 -sha256 -nodes -days 3650 \
  -keyout ca.key \
  -out ca.crt \
  -subj "/CN=Tabularis Trino Local Test CA" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"

openssl req -new -newkey rsa:2048 -nodes \
  -keyout https-server.key \
  -out https-server.csr \
  -subj "/CN=localhost" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature,keyEncipherment" \
  -addext "extendedKeyUsage=serverAuth" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"

openssl x509 -req -in https-server.csr \
  -CA ca.crt -CAkey ca.key -CAcreateserial \
  -out https-server.crt -days 825 -sha256 -copy_extensions copy

cat https-server.crt https-server.key > https-server.pem
chmod 644 https-server.pem
rm -f https-server.csr ca.srl

printf 'Created local Trino TLS files in %s\n' "$cert_dir"
