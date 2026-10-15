package com.acme.shop

import scala.util.*

enum Color:
  case Red, Green

trait Greeter:
  def greet(name: String): String

class Hello extends Greeter:
  def greet(name: String): String = s"hi $name"

given Greeter = new Hello

def top(x: Int): Int =
  x + 1
