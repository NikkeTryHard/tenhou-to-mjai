import sys
from pathlib import Path

# Add parent dir to path so tests can import journal
sys.path.insert(0, str(Path(__file__).parent.parent))
