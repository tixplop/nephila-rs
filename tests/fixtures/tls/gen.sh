#!/bin/sh
# Régénère les certificats de test (REQ-036). Nécessite openssl. Sortie commitée.
set -eu
cd "$(dirname "$0")"
DAYS=36500

ca() { # nom
  openssl ecparam -name prime256v1 -genkey -noout -out "$1.key.tmp"
  openssl pkcs8 -topk8 -nocrypt -in "$1.key.tmp" -out "$1.key"
  rm "$1.key.tmp"
  openssl req -new -x509 -key "$1.key" -sha256 -days $DAYS -subj "/CN=Nephila Test $1" \
    -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -out "$1.pem"
}

leaf() { # nom san
  openssl ecparam -name prime256v1 -genkey -noout -out "$1.key.tmp"
  openssl pkcs8 -topk8 -nocrypt -in "$1.key.tmp" -out "$1.key"
  rm "$1.key.tmp"
  openssl req -new -key "$1.key" -subj "/CN=$1" -out "$1.csr"
  printf 'basicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=%s\n' "$2" > "$1.ext"
  openssl x509 -req -in "$1.csr" -CA ca.pem -CAkey ca.key -CAcreateserial -sha256 -days $DAYS \
    -extfile "$1.ext" -out "$1.pem"
  rm "$1.csr" "$1.ext"
}

ca ca
ca other-ca
leaf server-ip "IP:127.0.0.1"
leaf server-name "DNS:nephila.test"
rm -f ca.srl

# bundle : clé privée, puis other-ca, puis ca
cat server-ip.key other-ca.pem ca.pem > bundle.pem
# bloc CERTIFICATE au base64 tronqué (longueur invalide)
{
  echo "-----BEGIN CERTIFICATE-----"
  sed -n '2p' ca.pem | cut -c1-31
  echo "-----END CERTIFICATE-----"
} > corrupt.pem
# clé seule
cp server-ip.key nocert.pem
