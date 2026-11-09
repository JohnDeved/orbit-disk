#!/usr/bin/env python3
"""Writes a throwaway Sparkle EdDSA key for `scripts/release.sh --dry-run` and prints its public key.

Usage: dry-run-sparkle-key.py NEW-KEY-FILE

The key file uses Sparkle's exported format (base64 of a 32-byte Ed25519 seed). The public key
is derived with the RFC 8032 reference arithmetic (Python 3.9's standard library has no Ed25519),
so the dry run can stamp it into its bundle's SUPublicEDKey: generate_appcast refuses to sign an
archive whose app does not trust the signing key. Never used for a real release.
"""
import base64
import hashlib
import os
import sys

P = 2**255 - 19
D = -121665 * pow(121666, P - 2, P) % P
Q = 2**252 + 27742317777372353535851937790883648493


def recover_x(y, sign):
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P)
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P:
        x = x * pow(2, (P - 1) // 4, P) % P
    return P - x if (x & 1) != sign else x


G_Y = 4 * pow(5, P - 2, P) % P
G_X = recover_x(G_Y, 0)
G = (G_X, G_Y, 1, G_X * G_Y % P)


def add(a, b):
    A = (a[1] - a[0]) * (b[1] - b[0]) % P
    B = (a[1] + a[0]) * (b[1] + b[0]) % P
    C = 2 * a[3] * b[3] * D % P
    Dd = 2 * a[2] * b[2] % P
    E, F, Gg, H = B - A, Dd - C, Dd + C, B + A
    return (E * F % P, Gg * H % P, F * Gg % P, E * H % P)


def multiply(scalar, point):
    result = (0, 1, 1, 0)
    while scalar:
        if scalar & 1:
            result = add(result, point)
        point = add(point, point)
        scalar >>= 1
    return result


def compress(point):
    inverse = pow(point[2], P - 2, P)
    x, y = point[0] * inverse % P, point[1] * inverse % P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def public_key(seed):
    digest = hashlib.sha512(seed).digest()
    scalar = int.from_bytes(digest[:32], "little")
    scalar &= (1 << 254) - 8
    scalar |= 1 << 254
    return compress(multiply(scalar, G))


def main():
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    seed = os.urandom(32)
    descriptor = os.open(sys.argv[1], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w") as file:
        file.write(base64.b64encode(seed).decode())
    print(base64.b64encode(public_key(seed)).decode())


if __name__ == "__main__":
    main()
