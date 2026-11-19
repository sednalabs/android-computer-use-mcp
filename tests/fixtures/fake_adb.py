#!/usr/bin/env python3
"""Deterministic simulated SDK; never contacts a real Android device."""
import sys, pathlib, json, shlex
root = pathlib.Path(__file__).resolve().parents[1]
args = sys.argv[1:]
with (root / 'calls.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\n')
if args and args[0] == 'devices':
    print('List of devices attached')
    if not (root / 'offline').exists():
        print('fixture-device device model:simulated_fixture')
    sys.exit(0)
if (root / 'offline').exists():
    sys.exit('The simulated device is offline')
if args[:2] != ['-s', 'fixture-device']:
    sys.exit('Only the simulated fixture-device is available')
args = args[2:]
if args and args[0] == 'shell':
    args = ['shell'] + (shlex.split(args[1]) if len(args) == 2 else args[1:])
command = ' '.join(args)
if command == 'wait-for-device':
    pass
elif command == 'shell getprop sys.boot_completed':
    print('1')
elif command == 'shell getprop ro.build.fingerprint':
    print('simulated-fixture/no-android-emulator')
elif command == 'shell pm path android':
    print('package:/system/framework/framework-res.apk')
elif command == 'shell cat /proc/sys/kernel/random/boot_id':
    print('11111111-1111-4111-8111-111111111111')
elif command == 'exec-out screencap -p':
    sys.stdout.buffer.write((root / 'screen.png').read_bytes())
elif command == 'exec-out uiautomator dump /dev/tty':
    sys.stdout.write((root / 'ui.xml').read_text())
elif command == 'shell dumpsys input':
    print('Viewport INTERNAL: displayId=0, orientation=0, valid=true')
elif command == 'shell wm size':
    print('Physical size: 1x1')
elif command in ['shell dumpsys window windows', 'shell dumpsys window']:
    print('mCurrentFocus=Window{a u0 fixture/.Main}')
elif command == 'shell dumpsys activity activities':
    print('mResumedActivity: ActivityRecord{a u0 fixture/.Main}')
elif command in ['shell input keyevent KEYCODE_HOME', 'shell input keyevent KEYCODE_BACK']:
    with (root / 'effects.jsonl').open('a') as log:
        log.write(json.dumps({'serial': 'fixture-device', 'command': command}) + '\n')
else:
    sys.exit('Unsupported simulated ADB request: ' + command)
