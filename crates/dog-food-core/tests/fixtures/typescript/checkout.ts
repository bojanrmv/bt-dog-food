// Fixture: a `dgf: N` comment states the expected complexity of the function
// that starts on the next line.

// dgf: 1
export function add(a: number, b: number): number {
  return a + b;
}

// dgf: 7 cognitive=9 nesting=3 params=2
export async function handleUpdate(req: Request, res: Response) {
  const { itemId, qty } = req.body;
  if (!itemId || qty == null) {
    return res.status(400).end();
  }
  try {
    const cart = await carts.get(req.userId);
    for (const line of cart.lines) {
      if (line.itemId === itemId) {
        line.qty = qty > 0 ? qty : 0;
      }
    }
    await carts.save(cart);
    res.json(cart);
  } catch (err) {
    res.status(500).end();
  }
}

class PriceCalculator {
  // dgf: 4
  discount(total: number, member: boolean): number {
    if (total > 100 && member) {
      return 0.15;
    }
    return total > 50 ? 0.05 : 0;
  }
}

// dgf: 4
export const label = (status: string): string => {
  switch (status) {
    case "new":
      return "New";
    case "paid":
      return "Paid";
    case "shipped":
      return "Shipped";
    default:
      return "Unknown";
  }
};
