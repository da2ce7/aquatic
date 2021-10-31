#/bin/bash

# Based on https://github.com/rustls/rustls/blob/0507dd0111a038516dde39f52f1175229d187788/bogo/regen-certs
# 
# ISC License (ISC)
# Copyright (c) 2016, Joseph Birr-Pixton <jpixton@gmail.com>
# 
# Permission to use, copy, modify, and/or distribute this software for
# any purpose with or without fee is hereby granted, provided that the
# above copyright notice and this permission notice appear in all copies.
# 
# THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
# WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED
# WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
# AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL
# DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR
# PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS
# ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
# THIS SOFTWARE.

set -e

mkdir -p tmp/tls

cd tmp/tls

rm -rf keys/ && mkdir -p keys/

# cert.pem/key.pem: rsa2048/sha256 self signed
openssl req -batch -x509 \
          -utf8 \
          -newkey rsa:2048 \
          -sha256 \
          -days 3650 \
          -nodes -keyout keys/key.pem \
          -out keys/cert.pem \
          -reqexts SAN \
          -extensions SAN \
          -config <(cat <<EOF
[req]
prompt=no
distinguished_name=req_distinguished_name
[req_distinguished_name]
O=bogo
[SAN]
subjectAltName=DNS:test,DNS:example.com
EOF
)

# rsa_1024_cert.pem/rsa_1024_key.pem: rsa1024/sha1 self signed
openssl req -batch -x509 \
          -utf8 \
          -newkey rsa:1024 \
          -sha1 \
          -days 3650 \
          -nodes -keyout keys/rsa_1024_key.pem \
          -out keys/rsa_1024_cert.pem \
          -reqexts SAN \
          -extensions SAN \
          -config <(cat <<EOF
[req]
prompt=no
distinguished_name=req_distinguished_name
[req_distinguished_name]
O=bogo-rsa1024
[SAN]
subjectAltName=DNS:test
EOF
)

# rsa_chain_cert.pem/rsa_chain_key.pem: rsa2048/sha256 with chain rsa2048/sha256
# nb. chain is not validated
openssl req -batch -x509 \
          -utf8 \
          -newkey rsa:2048 \
          -sha256 \
          -days 3650 \
          -nodes -keyout cakey.pem \
          -out cacert.pem
openssl req -batch -x509 \
          -utf8 \
          -newkey rsa:2048 \
          -sha256 \
          -days 3650 \
          -nodes -keyout keys/rsa_chain_key.pem \
          -out keys/rsa_chain_cert.pem \
          -reqexts SAN \
          -extensions SAN \
          -config <(cat <<EOF
[req]
prompt=no
distinguished_name=req_distinguished_name
[req_distinguished_name]
O=bogo-chain
[SAN]
subjectAltName=DNS:test
EOF
)
cat cacert.pem >> keys/rsa_chain_cert.pem

# ecdsa_p256_cert.pem/ecdsa_p256_key.pem: ecdsap256/sha1(?)
openssl req -batch -x509 \
          -utf8 \
          -newkey ec \
          -pkeyopt ec_paramgen_curve:prime256v1 \
          -sha1 \
          -days 3650 \
          -nodes -keyout keys/ecdsa_p256_key.pem \
          -out keys/ecdsa_p256_cert.pem \
          -reqexts SAN \
          -extensions SAN \
          -config <(cat <<EOF
[req]
prompt=no
distinguished_name=req_distinguished_name
[req_distinguished_name]
O=bogo-p256
[SAN]
subjectAltName=DNS:test
EOF
)

# ecdsa_p384_cert.pem/ecdsa_p384_key.pem: ecdsap384/sha1
openssl req -batch -x509 \
          -utf8 \
          -newkey ec \
          -pkeyopt ec_paramgen_curve:secp384r1 \
          -sha1 \
          -days 3650 \
          -nodes -keyout keys/ecdsa_p384_key.pem \
          -out keys/ecdsa_p384_cert.pem \
          -reqexts SAN \
          -extensions SAN \
          -config <(cat <<EOF
[req]
prompt=no
distinguished_name=req_distinguished_name
[req_distinguished_name]
O=bogo-p384
[SAN]
subjectAltName=DNS:test
EOF
)

echo "Making CA cert trusted system-wide.."
sudo cp cacert.pem /usr/local/share/ca-certificates/snakeoil.crt
sudo update-ca-certificates