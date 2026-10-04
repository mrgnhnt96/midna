import os, sys, time, random
cols, rows = os.get_terminal_size()
end = time.time() + float(sys.argv[1])
out = sys.stdout.buffer
chars = "abcdefghijklmnopqrstuvwxyz0123456789#@%&*"
while time.time() < end:
    buf = ["\x1b[H"]
    for y in range(rows):
        for x in range(cols):
            buf.append("\x1b[38;2;%d;%d;%dm\x1b[48;2;%d;%d;%dm%s" % (random.randrange(256), random.randrange(256), random.randrange(256), random.randrange(64), random.randrange(64), random.randrange(64), random.choice(chars)))
    out.write("".join(buf).encode()); out.flush()
out.write(b"\x1b[0m\x1b[2J\x1b[H"); out.flush()
