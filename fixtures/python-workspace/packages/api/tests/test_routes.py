import pytest

from shop_api import checkout


def test_checkout():
    assert checkout("t:checkout", 100) == {"total": 120}


def test_scope():
    with pytest.raises(PermissionError):
        checkout("t:admin", 100)
