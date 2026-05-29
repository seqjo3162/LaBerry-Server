# E2EE Decryption Fix Summary

## Problem Description

Users were seeing two types of E2EE decryption errors when viewing their own messages:

1. **"Не удалось расшифровать сообщение на этом устройстве"** (Failed to decrypt message on this device) - OperationError
2. **"Сообщение зашифровано не для этого аккаунта или устройства"** (Message encrypted for different account or device)

These errors occurred even when users were logged into the same account that sent the messages.

## Root Cause Analysis

The E2EE implementation had a **device ID mismatch** issue:

### During Encryption:
1. When encrypting a message for oneself (in DM chats), the code fetches device keys from the server via `e2eeGetUserDeviceKeys(currentUser.id)`
2. If this API call returns empty (due to race conditions, server issues, or no registered devices), it falls back to using the account-level public key with `device_id: 'server'`
3. The wrapped message key is stored under `keys[currentUser.id]['server']`

### During Decryption:
1. The code looks for a wrapped key using the current session's device ID (a UUID from `e2eeGetOrCreateDeviceId()`)
2. It only checked for: `userKeyMap[myDeviceId]`, `userKeyMap[myDeviceId]` (redundant), and `userKeyMap['unknown']`
3. **It never checked for 'server'**, causing it to fail to find the wrapped key

This mismatch caused:
- "Сообщение зашифровано не для этого аккаунта или устройства" when the wrapped key wasn't found
- "Не удалось расшифровать сообщение на этом устройстве" (OperationError) when it found a wrapped key but for a different device, leading to decryption failure

## Implemented Fixes

### 1. Decryption Path: Added 'server' Device ID Check

**File:** `app.js` (around line 656-660)

**Before:**
```javascript
let wrapped = userKeyMap[String(myDeviceId)] || userKeyMap[myDeviceId] || userKeyMap['unknown'];
```

**After:**
```javascript
let wrapped = userKeyMap[String(myDeviceId)] || userKeyMap[myDeviceId] || userKeyMap['server'] || userKeyMap['unknown'];
```

This ensures that wrapped keys stored under the 'server' device ID (from the fallback path during encryption) can be found during decryption.

### 2. Encryption Path: Added Current Session Device

**File:** `app.js` (around line 520-530)

**Added:**
```javascript
const myDeviceId = e2eeGetOrCreateDeviceId();
const myPublicJwk = identity.publicJwk;

// ... inside recipient loop ...

// For current user, also include current session's device to ensure self-decryption works
const isCurrentUser = id === Number(currentUser?.id);
if (isCurrentUser && myDeviceId && myPublicJwk) {
    // Check if current session device is already in the list
    const alreadyIncluded = devices.some(d => String(d.device_id) === String(myDeviceId));
    if (!alreadyIncluded) {
        devices = [{ device_id: myDeviceId, public_jwk: myPublicJwk }, ...devices];
    }
}
```

This ensures that when encrypting for oneself, the current session's device ID and public key are explicitly included in the recipient list, even if the server hasn't returned the device keys yet or if there was a race condition.

### 3. Enhanced Debugging

**File:** `app.js` (multiple locations)

Added detailed error logging:
- Log which device IDs are present when a wrapped key is not found
- Log whether ephemeral and sender keys are present when wrap key derivation fails
- Distinguish between inner and outer decryption failures

This helps diagnose any remaining issues.

### 4. Added Missing HKDF Function

**File:** `app.js` (around line 160)

Added the missing `e2eeHkdf` function that was referenced by the X25519 key derivation code but was undefined. This prevents errors in the X25519 code path.

## How This Fixes the Issue

### For New Messages:
1. When a user sends a message to themselves, the encryption now explicitly includes the current session's device ID
2. The wrapped key is stored under both the current device ID and potentially 'server' (fallback)
3. During decryption, the code checks for both the current device ID and 'server'
4. This ensures the wrapped key can be found and decryption succeeds

### For Existing Messages:
1. Messages encrypted before this fix that used the 'server' fallback device ID can now be decrypted
2. The code checks for 'server' explicitly in the decryption path

## Testing Recommendations

After applying these fixes:

1. **Clear browser cache** to ensure the updated JavaScript is loaded
2. **Test in a DM chat:**
   - Send a new message
   - Refresh the page
   - Verify the message can be decrypted
3. **Test in a server channel:**
   - Send messages in a server channel
   - Verify they can be decrypted
4. **Check old messages:**
   - Verify that previously encrypted messages can now be decrypted

## Technical Details

### ECDH Key Derivation Flow

**Encryption (for each recipient):**
```
wrapKey = deriveKey(recipient.publicKey, ephemeral.privateKey)
messageKeyWrapped = encrypt(wrapKey, messageKey)
store in envelope: keys[recipientId][deviceId] = {iv, ct}
```

**Decryption:**
```
wrapKey = deriveKey(ephemeral.publicKey, recipient.privateKey)
messageKey = decrypt(wrapKey, messageKeyWrapped)
```

ECDH is commutative, so:
`deriveKey(A.public, B.private) == deriveKey(B.public, A.private)`

This ensures that both parties can derive the same shared secret.

### Device ID Consistency

The fix ensures that:
- The device ID used during encryption matches the device ID used during decryption
- Both 'server' and explicit device IDs are checked during decryption
- The current session's device is explicitly included when encrypting for oneself

## Files Modified

- `D:\LaBerry-Server\server\static\js\app.js`
  - Added `e2eeHkdf` function (for X25519 support)
  - Modified `e2eeEncryptForCurrentChat` to include current session device
  - Modified `e2eeDecryptText` to check for 'server' device ID
  - Enhanced error logging in E2EE functions

## Remaining Considerations

1. **Old messages encrypted before device key support:** These used a different format and may still not be decryptable. This fix addresses messages encrypted with the current format.

2. **Multiple device support:** This fix ensures basic self-decryption works. For full multi-device support, additional testing is needed to ensure messages encrypted on one device can be decrypted on another device.

3. **X25519 keys:** The added `e2eeHkdf` function provides basic X25519 support, but the X25519 code path should be tested separately.

4. **Key persistence:** Users should be aware that clearing browser data will lose E2EE keys, making old messages undecryptable. This is expected behavior for end-to-end encryption.
