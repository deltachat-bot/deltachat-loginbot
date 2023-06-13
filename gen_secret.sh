cat /dev/urandom | tr -dc 'A-Za-z0-9' | fold -w 32 | head -n 2;
