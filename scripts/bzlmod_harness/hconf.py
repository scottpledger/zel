"""Shared harness configuration. Set ZEL_HARNESS_DIR to relocate all scratch state."""
import os, tempfile
VBZ = os.environ.get('ZEL_HARNESS_DIR') or os.path.join(tempfile.gettempdir(), 'zel-bzlmod-harness')
PORT = os.environ.get('ZEL_HARNESS_PORT') or '18765'
