void process(int n) {
  if (n > 0) {
    for (var i = 0; i < n; i++) {
      continue;
    }
  }
  while (n > 0) {
    break;
  }
  switch (n) {
    case 1:
      return;
  }
  try {
    throw Exception("boom");
  } on FormatException catch (e) {
    print(e);
  } catch (err) {
    rethrow;
  }
}
