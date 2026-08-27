#!/usr/bin/env bash
set -euo pipefail

identity_name="${RT_TRANSLATION_CODESIGN_IDENTITY:-Realtime Translation Local Code Signing}"
if security find-identity -v -p codesigning | grep -Fq "\"$identity_name\""; then
  echo "Code signing identity already exists: $identity_name"
  exit 0
fi

keychain_path="$(security default-keychain -d user | sed -e 's/^[[:space:]]*"//' -e 's/"[[:space:]]*$//')"
temporary_directory="$(mktemp -d /private/tmp/realtime-translation-signing.XXXXXX)"
certificate_path="$temporary_directory/certificate.pem"
private_key_path="$temporary_directory/private-key.pem"
identity_path="$temporary_directory/identity.p12"
identity_password="$(/usr/bin/openssl rand -hex 24)"

cleanup() {
  rm -f "$identity_path" "$private_key_path" "$certificate_path"
  rmdir "$temporary_directory" 2>/dev/null || true
}
trap cleanup EXIT

/usr/bin/openssl req \
  -new \
  -newkey rsa:2048 \
  -x509 \
  -sha256 \
  -days 3650 \
  -nodes \
  -subj "/CN=$identity_name/O=Realtime Translation Local Development" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=codeSigning" \
  -addext "subjectKeyIdentifier=hash" \
  -keyout "$private_key_path" \
  -out "$certificate_path"

/usr/bin/openssl pkcs12 \
  -export \
  -out "$identity_path" \
  -inkey "$private_key_path" \
  -in "$certificate_path" \
  -name "$identity_name" \
  -passout "pass:$identity_password"

security import "$identity_path" \
  -k "$keychain_path" \
  -P "$identity_password" \
  -T /usr/bin/codesign \
  -T /usr/bin/security
security add-trusted-cert \
  -r trustRoot \
  -p codeSign \
  -k "$keychain_path" \
  "$certificate_path"

if ! security find-identity -v -p codesigning | grep -Fq "\"$identity_name\""; then
  echo "Failed to create code signing identity: $identity_name" >&2
  exit 1
fi

echo "Created stable local code signing identity: $identity_name"
