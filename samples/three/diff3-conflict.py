def price_with_tax(price, region):
<<<<<<< HEAD
    rate = TAX_RATES.get(region, 0.20)
    return round(price * (1 + rate), 2)
||||||| base
    rate = TAX_RATES.get(region, 0.2)
    return price * (1 + rate)
=======
    rate = TAX_RATES[region]
    return price * (1 + rate)
>>>>>>> feature/strict-regions


def discount(price, percent):
    if not 0 <= percent <= 100:
        raise ValueError("percent must be between 0 and 100")
    return price * (100 - percent) / 100


def shipping(weight_kg):
<<<<<<< HEAD
    return 4.90 if weight_kg < 2 else 9.90
||||||| base
    return 4.90
=======
    return 4.90 + 1.50 * max(0, weight_kg - 2)
>>>>>>> feature/strict-regions
