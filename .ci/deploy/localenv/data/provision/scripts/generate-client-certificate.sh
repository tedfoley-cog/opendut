#!/bin/bash
set -e
set -x

OPENDUT_CERT_CA_NAME="opendut-ca"
PROVISION_ROOT_DIR="/provision/"
PROVISION_PKI_DIR="$PROVISION_ROOT_DIR/pki/"
OPENDUT_PASSWORD_FILE="$PROVISION_ROOT_DIR/.env-pki"
OPENDUT_ENV_FILE="$PROVISION_ROOT_DIR/.env"
CA_PATH="$PROVISION_PKI_DIR/$OPENDUT_CERT_CA_NAME"

CA_BASENAME="$(basename "$CA_PATH")"
if [[ -f "$CA_PATH.key" && -f "$CA_PATH.pem" ]]; then
  CLIENT_NAME="${CA_BASENAME}-auth-client"
  CLIENT_CERT_PATH="$PROVISION_PKI_DIR/$CLIENT_NAME"
  CLIENT_DEPLOY_PATH="$PROVISION_PKI_DIR/deploy/$CLIENT_NAME"

  openssl req -new -sha512 -passout file:"$OPENDUT_PASSWORD_FILE" -out "$CLIENT_CERT_PATH".csr -newkey rsa:4096 -keyout "$CLIENT_CERT_PATH".key -subj "/CN=$CLIENT_NAME/C=XX/ST=Some-State/O=ExampleOrg"

  cat > "$CLIENT_CERT_PATH".v3.ext << EOF
authorityKeyIdentifier=keyid,issuer
basicConstraints=CA:FALSE
keyUsage = digitalSignature, nonRepudiation, keyEncipherment, dataEncipherment
extendedKeyUsage = clientAuth
subjectAltName = @alt_names
[alt_names]
DNS.1 = $CLIENT_NAME
EOF


SERIAL_FILE="$CA_PATH".srl
 if [ -f "$SERIAL_FILE" ]; then
    openssl x509 -req -in "$CLIENT_CERT_PATH".csr -CA "$CA_PATH".pem -CAkey "$CA_PATH".key -passin file:"$OPENDUT_PASSWORD_FILE" -CAserial "$SERIAL_FILE" -outform PEM -out "$CLIENT_CERT_PATH".pem -days 9999 -sha256 -extfile "$CLIENT_CERT_PATH".v3.ext
 else
    openssl x509 -req -in "$CLIENT_CERT_PATH".csr -CA "$CA_PATH".pem -CAkey "$CA_PATH".key -passin file:"$OPENDUT_PASSWORD_FILE" -CAcreateserial -outform PEM -out "$CLIENT_CERT_PATH".pem -days 9999 -sha256 -extfile "$CLIENT_CERT_PATH".v3.ext
 fi

cp "$CLIENT_CERT_PATH".pem "$CLIENT_DEPLOY_PATH".pem
openssl rsa -in "$CLIENT_CERT_PATH".key -passin file:"$OPENDUT_PASSWORD_FILE" -out "$CLIENT_DEPLOY_PATH".key

openssl x509 -in "$CLIENT_CERT_PATH".pem -out "$CLIENT_DEPLOY_PATH".crt -outform PEM

rm "$CLIENT_CERT_PATH".csr
rm "$CLIENT_CERT_PATH".v3.ext