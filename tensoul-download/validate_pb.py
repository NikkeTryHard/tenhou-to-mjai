import sys
import os

# Ensure we can find the local packages
sys.path.append('/home/nikketryhard/dev/tenhou-to-mjai/tensoul-download')

import ms.protocol_pb2 as pb
import json
from tensoul.downloader import MajsoulPaipuDownloader

pb_path = '/home/nikketryhard/dev/tenhou-to-mjai/tensoul-download/jade-raw/231219_31263425_78aa_41b5_86f1_3c4e9ae1457f.pb'

with open(pb_path, 'rb') as f:
    data = f.read()

res = pb.ResGameRecord()
res.ParseFromString(data)

dl = MajsoulPaipuDownloader()
dl.version_to_force = '0.11.216'
result = dl._handle_game_record(res, 0)

# Deep validation of Tenhou JSON
print("=== FIELD VALIDATION ===")

# 1. ver
assert result['ver'] == '2.3', f"Bad ver: {result['ver']}"
print(f"ver: {result['ver']} OK")

# 2. ref (game UUID)
assert len(result['ref']) > 30, f"Bad ref length: {len(result['ref'])}"
print(f"ref: {result['ref']} OK")

# 3. ratingc
assert result['ratingc'] in ['PF3', 'PF4'], f"Bad ratingc: {result['ratingc']}"
nplayers = int(result['ratingc'][-1])
print(f"ratingc: {result['ratingc']} ({nplayers} players) OK")

# 4. rule
rule = result['rule']
assert 'disp' in rule
assert 'aka53' in rule
assert 'aka52' in rule  
assert 'aka51' in rule
print(f"rule: {rule} OK")

# 5. lobby
print(f"lobby: {result['lobby']} OK")

# 6. dan - should be nplayers entries
assert len(result['dan']) == nplayers, f"Bad dan count: {len(result['dan'])}"
print(f"dan: {result['dan']} OK")

# 7. rate
assert len(result['rate']) == nplayers
print(f"rate: {result['rate']} OK")

# 8. sx
assert len(result['sx']) == nplayers
print(f"sx: {result['sx']} OK")

# 9. name
assert len(result['name']) == nplayers
assert all(isinstance(n, str) and len(n) > 0 for n in result['name'])
print(f"name: {result['name']} OK")

# 10. sc - should be nplayers*2 entries (alternating score, point)
assert len(result['sc']) == nplayers * 2, f"Bad sc length: {len(result['sc'])}"
print(f"sc: {result['sc']} OK")

# 11. title
assert len(result['title']) == 2
print(f"title: {result['title']} OK")

# 12. log - array of rounds, each round is an array
assert len(result['log']) > 0, "No rounds!"
for i, round_data in enumerate(result['log']):
    assert isinstance(round_data, list), f"Round {i} is not a list"
    # Each round should have at least the basic structure
    # [0]: round info [ba, honba, riichi_sticks]
    # [1]: starting scores
    # [2]-[5]: initial hands (or [2]-[4] for sanma)
    assert len(round_data) >= 10, f"Round {i} too short: {len(round_data)}"
print(f"log: {len(result['log'])} rounds, all valid OK")

# 13. playerMapping
assert len(result['playerMapping']) == nplayers
for pm in result['playerMapping']:
    assert 'nickname' in pm
    assert 'account_id' in pm
print(f"playerMapping: {result['playerMapping']} OK")

# Check round structure detail on first round
r0 = result['log'][0]
print(f"\n=== ROUND 0 DETAIL ===")
print(f"Round info: {r0[0]}")  # [ba, honba, riichi]
print(f"Starting scores: {r0[1]}")  # [25000, 25000, 25000, 25000]
print(f"Dora indicator: {r0[2]}")  # [tile_id]
print(f"Total elements in round: {len(r0)}")

# Save full output for inspection
with open('/tmp/tenhou_full.json', 'w') as f:
    json.dump(result, f, ensure_ascii=False, indent=2)
print(f"\nSaved full JSON to /tmp/tenhou_full.json ({len(json.dumps(result))} bytes)")
print("\n=== ALL VALIDATIONS PASSED ===")
