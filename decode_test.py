import math

def base_to_int(s, alphabet):
    res = 0
    base = len(alphabet)
    for char in s:
        res = res * base + alphabet.index(char)
    return res

base62 = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
base64_url = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
# amae-koromo might use a different order or set

samples = [
    ("8sh3swwKqPR", 1735705861),
    ("8sh45KA7IBy", 1735706025),
    ("8sgkIwyYfD5", None) # full_uuid: 250101-a7d2bfbf-dac8-45b9-a667-861f82589725
]

for s, ts in samples:
    print(f"Sample: {s}")
    try:
        val62 = base_to_int(s, base62)
        print(f"  Base62: {val62} (hex: {hex(val62)})")
    except ValueError:
        print("  Base62: Invalid")
        
    try:
        # Standard B64 is usually different, but let's try a few
        # Most 11-char IDs in systems like YouTube use a variant of B64
        # 11 chars * 6 bits = 66 bits.
        pass
    except:
        pass

# Specifically look at 8sgkIwyYfD5 -> 250101-a7d2bfbf-dac8-45b9-a667-861f82589725
# Maybe the 11 chars are the FIRST part of the UUID?
# a7d2bfbf-dac8...
# a7d2bfbf (hex) = 2815672255 (decimal)
# 8sgkIwyYfD5 (Base62) = ?
