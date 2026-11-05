import pytest
def test_pass(): assert True
def test_fail(): assert False, "intentional failure"
def test_error_fixture(): pass
@pytest.fixture(autouse=True)
def setup_error(request):
    if request.node.name == "test_error_fixture": raise RuntimeError("intentional error")
@pytest.mark.skip(reason="intentional skip")
def test_skip(): pass
