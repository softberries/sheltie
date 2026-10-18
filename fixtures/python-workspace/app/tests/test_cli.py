from shop_app.cli import main


def test_main(capsys):
    assert main(["100"]) == 0
    assert capsys.readouterr().out == "120\n"
