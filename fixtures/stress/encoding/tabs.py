# tab-indented
def main():
	xs = [(i * 7919) % 2000 for i in range(1800)]
	n = len(xs)
	for i in range(n):
		for j in range(n - 1 - i):
			if xs[j] > xs[j + 1]:
				xs[j], xs[j + 1] = xs[j + 1], xs[j]
	print(xs[:5], xs[-5:])


main()
