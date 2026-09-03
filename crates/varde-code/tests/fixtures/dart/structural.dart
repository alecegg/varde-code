import 'helper.dart';
export 'api.dart';

mixin Logging {
  void log(String msg) {
    print(msg);
  }
}

abstract class Animal extends Base with Logging implements Comparable<Animal> {
  final String name;
  int legs = 4;
  static const int maxLegs = 8;

  Animal(this.name);

  void speak(int volume) {
    var greeting = "hello";
    const pi = 3.14;
    bool loud = true;
    repo.save();
    log(greeting);
  }
}

int add(int a, int b) => a + b;
