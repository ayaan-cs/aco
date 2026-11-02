import time

total = sum(i * i for i in range(200000))
print(total, time.time())
