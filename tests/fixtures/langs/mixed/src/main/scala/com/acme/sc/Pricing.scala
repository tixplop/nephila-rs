package com.acme.sc

trait Pricing {
  def price(): Int
}

case class Discount(rate: Int) extends Pricing {
  def price(): Int = rate
}

object Defaults
