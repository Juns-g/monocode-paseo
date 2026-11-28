// node:crypto stand-in for the exact @noble surface the Paseo relay Noise handshake uses
// (packages/relay/src/noise/*). bundle.mjs maps @noble/* here only when the Paseo source checkout
// has no installed @noble packages. Verified against RFC 7748 / RFC 8439 vectors in noble-shim.test.mjs.
import {createHash,createHmac,createCipheriv,createDecipheriv,createPrivateKey,createPublicKey,diffieHellman,randomBytes} from 'node:crypto';

const bytes=value=>new Uint8Array(value.buffer,value.byteOffset,value.byteLength);
export const sha256=Object.assign(data=>bytes(createHash('sha256').update(data).digest()),{outputLen:32,blockLen:64});
export function hmac(hash,key,message){
  if(hash!==sha256)throw Error('Only hmac(sha256) is supported');
  return bytes(createHmac('sha256',key).update(message).digest());
}

const PKCS8=Buffer.from('302e020100300506032b656e04220420','hex'),SPKI=Buffer.from('302a300506032b656e032100','hex');
const privateKey=secret=>{if(secret?.length!==32)throw Error('X25519 secret key must be 32 bytes');return createPrivateKey({key:Buffer.concat([PKCS8,secret]),format:'der',type:'pkcs8'});};
export const x25519={
  utils:{randomSecretKey:()=>bytes(randomBytes(32))},
  getPublicKey:secret=>bytes(createPublicKey(privateKey(secret)).export({format:'der',type:'spki'}).subarray(SPKI.length)),
  getSharedSecret(secret,publicKey){
    if(publicKey?.length!==32)throw Error('X25519 public key must be 32 bytes');
    const shared=diffieHellman({privateKey:privateKey(secret),publicKey:createPublicKey({key:Buffer.concat([SPKI,publicKey]),format:'der',type:'spki'})});
    if(shared.every(b=>b===0))throw Error('X25519 shared secret is all zero'); // Low-order point, as @noble rejects.
    return bytes(shared);
  },
};

// ChaCha20-Poly1305 (RFC 8439): ciphertext || 16-byte tag, like @noble/ciphers.
export function chacha20poly1305(key,nonce,aad){
  const options={authTagLength:16};
  return {
    encrypt(plaintext){
      const cipher=createCipheriv('chacha20-poly1305',key,nonce,options);
      if(aad?.length)cipher.setAAD(aad,{plaintextLength:plaintext.length});
      return bytes(Buffer.concat([cipher.update(plaintext),cipher.final(),cipher.getAuthTag()]));
    },
    decrypt(ciphertext){
      if(ciphertext.length<16)throw Error('Invalid ChaCha20-Poly1305 ciphertext');
      const decipher=createDecipheriv('chacha20-poly1305',key,nonce,options);
      const body=ciphertext.subarray(0,ciphertext.length-16);
      decipher.setAuthTag(ciphertext.subarray(ciphertext.length-16));
      if(aad?.length)decipher.setAAD(aad,{plaintextLength:body.length});
      return bytes(Buffer.concat([decipher.update(body),decipher.final()]));
    },
  };
}
