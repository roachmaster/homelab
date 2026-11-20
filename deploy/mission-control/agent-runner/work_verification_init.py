"""Pure launcher recipe for a verifier's private PID namespace.

PID 1 must reap orphaned descendants while its owned verifier is still alive.
The fixed script runs after the supervisor's UID/capability drop and forwards
termination to its owned child session. It never selects a command from logs.
"""

VERIFICATION_INIT = r'''
import os,signal,subprocess,sys,time
child = None
received = 0

def forward(signum, _frame):
 global received
 received = received or signum
 if child is not None:
  try: os.killpg(child.pid, signum)
  except ProcessLookupError: pass

for signum in (signal.SIGTERM,signal.SIGINT,signal.SIGHUP):
 signal.signal(signum,forward)
try:
 child = subprocess.Popen(sys.argv[1:],start_new_session=True)
except OSError:
 sys.exit(127)
if received:
 forward(received,None)
main_status = None
cleanup_at = None
while True:
 any_children = True
 while True:
  try: pid,status = os.waitpid(-1,os.WNOHANG)
  except ChildProcessError:
   any_children = False
   break
  if pid == 0:
   break
  if pid == child.pid:
   main_status = os.waitstatus_to_exitcode(status)
   child.returncode = main_status
 if main_status is not None:
  if not any_children:
   break
  if cleanup_at is None:
   cleanup_at = time.monotonic()
   try: os.killpg(child.pid,signal.SIGTERM)
   except ProcessLookupError: pass
  age = time.monotonic()-cleanup_at
  if age >= .2:
   try: os.killpg(child.pid,signal.SIGKILL)
   except ProcessLookupError: pass
  if age >= 2:
   sys.exit(125)
 time.sleep(.005)
if received:
 sys.exit(128+received)
sys.exit(128-main_status if main_status < 0 else main_status)
'''


def verification_argv(command):
    """Wrap only an operator-selected argv, preserving its exit status."""
    if not isinstance(command, tuple) or not command or any(
        not isinstance(part, str) or not part or "\0" in part for part in command
    ):
        raise ValueError("Verification needs a fixed command argv")
    return ("/usr/bin/python3", "-B", "-c", VERIFICATION_INIT, *command)
