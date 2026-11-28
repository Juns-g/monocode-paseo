import test from 'node:test';
import assert from 'node:assert/strict';
import {sha256, hmac, x25519, chacha20poly1305} from './noble-shim.mjs';

const hex = s => Uint8Array.from(Buffer.from(s.replace(/\s+/g, ''), 'hex'));
const toHex = b => Buffer.from(b).toString('hex');

test('sha256 and hmac-sha256 match FIPS 180-2 / RFC 4231 vectors', () => {
  assert.equal(toHex(sha256(new TextEncoder().encode('abc'))), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  assert.equal(sha256.outputLen, 32);
  assert.equal(toHex(hmac(sha256, new TextEncoder().encode('Jefe'), new TextEncoder().encode('what do ya want for nothing?'))),
    '5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843');
  assert.throws(() => hmac(() => {}, new Uint8Array(1), new Uint8Array(1)), /sha256/);
});

test('x25519 matches RFC 7748 §6.1 and rejects low-order points', () => {
  const alice = hex('77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a');
  const bob = hex('5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb');
  const alicePub = x25519.getPublicKey(alice), bobPub = x25519.getPublicKey(bob);
  assert.equal(toHex(alicePub), '8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a');
  assert.equal(toHex(bobPub), 'de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f');
  const shared = '4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742';
  assert.equal(toHex(x25519.getSharedSecret(alice, bobPub)), shared);
  assert.equal(toHex(x25519.getSharedSecret(bob, alicePub)), shared);
  assert.equal(x25519.utils.randomSecretKey().length, 32);
  assert.throws(() => x25519.getSharedSecret(alice, new Uint8Array(32)));
  assert.throws(() => x25519.getSharedSecret(alice, new Uint8Array(31)), /32 bytes/);
});

test('chacha20poly1305 matches RFC 8439 §2.8.2 and rejects tampering', () => {
  const key = hex('808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f');
  const nonce = hex('070000004041424344454647');
  const aad = hex('50515253c0c1c2c3c4c5c6c7');
  const plaintext = new TextEncoder().encode("Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.");
  const sealed = chacha20poly1305(key, nonce, aad).encrypt(plaintext);
  assert.equal(sealed.length, plaintext.length + 16);
  assert.equal(toHex(sealed.subarray(0, 16)), 'd31a8d34648e60db7b86afbc53ef7ec2');
  assert.equal(toHex(sealed.subarray(sealed.length - 16)), '1ae10b594f09e26a7e902ecbd0600691');
  assert.deepEqual(chacha20poly1305(key, nonce, aad).decrypt(sealed), plaintext);
  const tampered = Uint8Array.from(sealed); tampered[3] ^= 1;
  assert.throws(() => chacha20poly1305(key, nonce, aad).decrypt(tampered));
  assert.throws(() => chacha20poly1305(key, nonce, new Uint8Array(0)).decrypt(sealed));
  // Empty AAD round trip (the relay transport form).
  const plain = chacha20poly1305(key, nonce).encrypt(new Uint8Array([1, 2, 3]));
  assert.deepEqual(chacha20poly1305(key, nonce).decrypt(plain), new Uint8Array([1, 2, 3]));
});
