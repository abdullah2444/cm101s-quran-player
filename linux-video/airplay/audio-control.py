#!/usr/bin/env python3
"""Called by the AirPlay receiver just before opening the audio device."""
import json
import urllib.request

try:
    with urllib.request.urlopen('http://127.0.0.1:8790/airplay?act=claim_audio', timeout=8) as response:
        result = json.load(response)
    raise SystemExit(0 if result.get('claimed') else 1)
except Exception:
    raise SystemExit(1)
