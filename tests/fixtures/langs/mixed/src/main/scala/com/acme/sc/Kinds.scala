package com.acme.sc

class Plain {
  def hello(): Int = 1
}

enum Mode {
  case On, Off
}

trait A
trait B
class C extends A with B
