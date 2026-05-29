// Polyfill: алиасы для обратной совместимости с API tweetnacl
if (typeof nacl !== 'undefined' && nacl.scalarMult && !nacl.scalarBaseMult) {
  nacl.scalarBaseMult = function(n) { return nacl.scalarMult.base(n); };
}
